import {
  AlertCircle, BookOpen, BookOpenText, FilePenLine, FileText, GitFork, Link2,
  ListTree, Network, PanelRight, Paperclip, RotateCw, Save, Search, SplitSquareHorizontal, Wand2,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { lazy, Suspense, useCallback, useEffect, useEffectEvent, useMemo, useRef, useState } from "react";
import {
  FileCaption, FileDialog, FileDock, FileSidebar, FileTree, FileUnavailable,
  keepOnlyCleanClose, SidebarToggle, useFileDock, useFileSidebar, type FilePanelApi,
} from "@/ide-file-workspace";
import { folderSearchTarget, readNoteSearchTarget } from "@/ide-file-search";
import { readNoteWindowTarget } from "@/file-editor-popout";
import { useCheckoutFilePreview } from "@/checkout-file-preview";
import { containingFolder, fileContextActions, fileHistoryTarget, readNoteRenameTarget, readOpenNotesTarget, terminalFolderTarget, useCheckoutTextReader, workingDiffTarget } from "@/file-context-actions";
import { useCheckoutTreeActions } from "@/checkout-tree-actions";
import { useCheckoutConversationActions } from "@/checkout-conversation-actions";
import { changedItemPath, itemIncludesPath, useCheckoutItemOperations } from "@/checkout-item-operations";
import { useCheckoutFileComparison } from "@/checkout-file-comparison";
import { ContextActions, type MenuGroups } from "@/components/ui/context-actions";
import { checkoutFavoriteKey, reconcileFavoriteItems, setCheckoutFavorites, useCheckoutFavorites,
  type CheckoutNoteFavorite } from "@/checkout-favorites";
import { deliveryNote, exampleNotes, fileName, findFileText, normalizeRepositoryPath, type ExampleFile } from "@/ide-knowledge-data";
import {
  anchorLine, favoriteAnchorLine, getProperties, headings, noteAnchorExcerpt, noteAnchorIdentity, parseReadingAddress, readingLocalAddress, repairWikiAddress, resolveWiki, rewriteNoteReferences,
  updateProperty, wikiLinks, type NoteAnchorKind, type NoteLocation, type NoteTarget,
} from "@/knowledge-document";
import { AttachmentPreview, downloadNoteDiagramPng, noteAttachments, type NoteAttachment } from "@/knowledge-media";
import { noteWikilink, type NoteContextHit, type NoteSelection } from "@/knowledge-context";
import { ExtractNoteDialog } from "@/knowledge-note-extraction";
import { RepositoryNoteMerge } from "@/repository-note-merge";
import { RepositoryTagRename } from "@/repository-tag-rename";
import type { NoteJump } from "@/knowledge-editor";
import { useRepositoryNotes, type NoteBuffer, type NoteRevision } from "@/repository-note-buffers";
import { downloadRepositoryAttachment, readExactRepositoryAttachment, RepositoryNoteMedia } from "@/repository-note-media";
import { NotePropertyEditor } from "@/note-property-editor";
import { ResizablePanel } from "@/panel-resize";
import { KnowledgeLinks, KnowledgeOutline, KnowledgeSearch, type KnowledgeLocation, type KnowledgeOutgoingLink } from "@/knowledge-navigation";
import { useRepositoryNoteIndex } from "@/repository-note-index";
import { RepositoryNoteRename } from "@/repository-note-rename";
import { RepositoryNotePlanning, useRepositoryNotePlans } from "@/repository-note-planning";
import { WorkflowLaunchPicker } from "@/workflow-launch";
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
  | { kind: "extract"; path: string; selection: NoteSelection; ownerPanel?: FilePanelApi }
  | { kind: "merge"; source: string; destination?: string; ownerPanel?: FilePanelApi }
  | { kind: "tagRename"; tag: string; ownerPanel?: FilePanelApi }
  | { kind: "conflict"; path: string; external: string; ownerPanel?: FilePanelApi }
  | { kind: "rename"; path: string }
  | { kind: "repositoryRename"; path: string; revision: string }
  | { kind: "launch"; source: NoteRevision }
  | { kind: "plan"; source: NoteRevision }
  | { kind: "linkedTasks" }
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

function resolveExampleLink(reference: NoteTarget, from: string, catalog: readonly ExampleFile[]): IndexedLinkResolution {
  if (noteAttachments.some(({ name }) => name === reference.target)) return { status: "attachment", paths: [reference.target] };
  const candidates = resolveWiki(reference, from, catalog).map(({ path, anchor }) => ({
    path, anchor: anchor ?? null,
    line: anchor ? anchorLine(catalog.find((file) => file.path === path)?.content ?? "", anchor) ?? null : null,
  }));
  if (candidates.length > 1) return { status: "ambiguous", candidates };
  const location = candidates[0];
  if (!location) return { status: "missing" };
  return { status: location.anchor && !location.line ? "missing_anchor" : "resolved", location };
}

export default function KnowledgeWorkspace(props: WorkspaceProps) {
  return <KnowledgeSurface key={`${props.checkout.checkoutId}:${props.simulation?.epoch ?? "real"}`} {...props} />;
}

function KnowledgeSurface({ checkout, target, simulation, startSimulation, openWorkspace }: WorkspaceProps) {
  const simulated = Boolean(simulation);
  const [reveal, setReveal] = useState<{ path: string }>();
  const initialWindowPath = target ? readNoteWindowTarget(target) : undefined;
  const initialTarget = target && !initialWindowPath ? readNoteSearchTarget(target) : undefined;
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
  const [mode, setMode] = useState<NoteMode>("live");
  const [editAlongsideReading, setEditAlongsideReading] = useState<ReadonlySet<string>>(() => new Set());
  const [jumps, setJumps] = useState<Record<string, NoteJump>>({});
  const [dialog, setDialog] = useState<KnowledgeDialog>();
  const [confirmingClose, setConfirmingClose] = useState(false);
  const [destination, setDestination] = useState("");
  const [status, setStatus] = useState(simulation?.scenario === "recovery" ? "Recovered an unsaved Markdown buffer." : "");
  const [problem, setProblem] = useState("");
  const [selection, setSelection] = useState("");
  const [selectedAnchor, setSelectedAnchor] = useState("");
  const lastTarget = useRef(simulated && !initialTarget?.line && !initialWindowPath ? target : undefined);
  const lastOpen = useRef(0);
  const { load: loadNote, watch: watchNote, getNote } = repository;
  const favoritesId = simulated ? "example" : checkout.checkoutId;
  const { favorites } = useCheckoutFavorites(favoritesId);
  const favoriteKeys = useMemo(() => new Set(favorites.map(checkoutFavoriteKey)), [favorites]);
  const active = dock.path;
  const note = active ? notes[active] : undefined;
  const savedNote = active ? repository.notes[active] : undefined;
  const canLaunchFromNote = Boolean(savedNote && !savedNote.sourceProblem && !isDirty(savedNote));
  const notePlans = useRepositoryNotePlans(checkout.projectId, savedNote && !savedNote.sourceProblem ? savedNote.base : undefined);
  const linkedTasks = [...new Map(notePlans.data?.flatMap(({ task }) => task ? [[task.id, task] as const] : [])).values()];
  const sampleDeliveryNote = simulation && active === deliveryNote;
  const showPlanAction = savedNote || sampleDeliveryNote && simulation.delivery === "note";
  const showLinkedTask = linkedTasks.length > 0 || sampleDeliveryNote && simulation.delivery !== "note";
  const canPlan = savedNote ? canLaunchFromNote : !isDirty(note);
  const conflictRecovery = dialog?.kind === "conflict" ? repository.notes[dialog.path]?.recovery : undefined;
  const source = note?.content ?? "";
  const editorMode = (path: string) => mode === "reading" && editAlongsideReading.has(path) ? "live" : mode;
  const activeMode = dock.view === "reading" ? "reading" : active ? editorMode(active) : mode;
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
  const matches = simulated ? findFileText(catalog, query, true).map(({ path, line, excerpt }) => ({ path, line, snippet: excerpt })) : noteIndex.hits;
  const outgoing = wikiLinks(source);
  const exampleBacklinks = simulated && active && inspector === "links" ? catalog.flatMap(({ path, content }) => {
    if (path === active) return [];
    const inbound = wikiLinks(content).filter((link) => {
      const resolved = resolveWiki(link, path, catalog);
      return resolved.length === 1 && resolved[0]?.path === active;
    });
    return inbound.length ? [{ source: { path }, label: `${fileName(path)} · ${inbound.length}` }] : [];
  }) : [];
  const previewCatalog = useMemo(() => new Map(noteIndex.index.data?.map((note) => [note.path, note])), [noteIndex.index.data]);
  const previewNote = (path: string) => previewCatalog.get(path);
  const previewTarget = (location: IndexedLinkLocation) => {
    const note = previewNote(location.path);
    return { ...location, title: note?.title, snippet: note?.preview };
  };
  const describeLink = (path: string, reference: NoteTarget, resolution: IndexedLinkResolution | undefined,
    line?: number, wiki = true): KnowledgeOutgoingLink | undefined => {
    if (resolution?.status === "attachment") return;
    const view: KnowledgeOutgoingLink["resolution"] = resolution?.status === "resolved"
      ? { kind: "resolved", target: previewTarget(resolution.location) }
      : resolution?.status === "missing_anchor" ? { kind: "missing_anchor", target: previewTarget(resolution.location) }
        : resolution?.status === "ambiguous" ? { kind: "ambiguous", candidates: resolution.candidates.map(previewTarget) }
          : { kind: "missing" };
    return { label: reference.alias || reference.raw, source: { path, line }, rawTarget: reference.raw,
      reference: { ...reference, wiki }, resolution: view };
  };
  const outgoingLinks = active && inspector === "links" ? (
    simulated ? outgoing.map((reference) => describeLink(active, reference, resolveExampleLink(reference, active, catalog)))
      : isDirty(note) ? outgoing.map((reference) => describeLink(active, reference, lookupLink(reference, active)))
      : relationships.data?.outgoing.map(({ link, resolution }) => {
        const raw = link.target + (link.anchor ? `#${link.anchor}` : "") + (link.alias ? `|${link.alias}` : "");
        const parsed = link.syntax === "markdown" ? parseReadingAddress(readingLocalAddress(active, link.target)) : undefined;
        const reference = link.syntax === "wiki" ? { raw, target: link.target, alias: link.alias ?? undefined, anchor: link.anchor ?? undefined, embed: link.embed }
          : parsed && { ...parsed, alias: link.alias ?? undefined };
        return reference && describeLink(active, reference, resolution, link.line, link.syntax === "wiki");
      }) ?? []
  ).filter((link): link is KnowledgeOutgoingLink => Boolean(link)) : [];
  const backlinks = simulated ? exampleBacklinks : (relationships.data?.backlinks ?? []).map((link) => ({
    source: { path: link.path, line: link.line, title: link.title, snippet: previewNote(link.path)?.preview },
    label: link.label ?? undefined,
  }));

  const open = useCallback((path: string, line?: number, anchor?: string, presentation?: NoteMode | "side" | "window" | { groupId: string }) => {
    const focus = (file: string, targetLine?: number) => {
      if (targetLine || anchor) setJumps((before) => ({ ...before, [file]: { line: targetLine, anchor, token: (before[file]?.token ?? 0) + 1 } }));
      setGraph(false);
      if (typeof presentation === "object") dock.open(file, presentation.groupId);
      else if (presentation === "window") dock.openInWindow(file);
      else if (presentation === "side") {
        if (mode === "reading") setEditAlongsideReading((before) => new Set(before).add(file));
        dock.split(file, "reading");
      }
      else {
        if (presentation) {
          setMode(presentation);
          setEditAlongsideReading(new Set());
        }
        dock.open(file);
      }
    };
    if (simulated) { focus(path, line); return; }
    const request = ++lastOpen.current;
    void loadNote(path).then((file) => {
      if (!file || request !== lastOpen.current) return;
      const anchorTarget = anchor ? anchorLine(getNote(file)?.content ?? "", anchor) : undefined;
      focus(file, anchorTarget ?? line);
      watchNote(file);
    });
  }, [dock.open, dock.openInWindow, dock.split, getNote, loadNote, mode, simulated, watchNote]);

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
    if (!target || target === lastTarget.current || !dock.ready || readNoteRenameTarget(target) !== undefined || readOpenNotesTarget(target) !== undefined) return;
    const windowPath = readNoteWindowTarget(target);
    const location = windowPath === undefined ? readNoteSearchTarget(target) : { path: windowPath, line: undefined, side: false };
    const next = location && normalizeRepositoryPath(location.path);
    if (!next || !location) return;
    lastTarget.current = target;
    open(next, location.line, undefined, windowPath !== undefined ? "window" : location.side ? "side" : undefined);
  }, [dock.ready, open, target]);

  const update = (path: string, content: string) => {
    if (!simulated) { repository.update(path, content); return; }
    setNotes((before) => {
      const current = before[path];
      return current && current.content !== content ? { ...before, [path]: { ...current, content } } : before;
    });
  };
  const save = async (path: string, ownerPanel?: FilePanelApi) => {
    const external = (simulated ? notes[path] : getNote(path))?.external;
    if (external !== undefined) {
      dock.cancelCloseBatch();
      setDialog({ kind: "conflict", path, external, ownerPanel });
      return false;
    }
    if (!simulated) {
      const saved = await repository.save(path);
      if (saved) setStatus(`Saved ${path}`);
      else {
        const incoming = getNote(path)?.external;
        if (incoming !== undefined) {
          dock.cancelCloseBatch();
          setDialog({ kind: "conflict", path, external: incoming, ownerPanel });
        }
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
    const reading = dock.api?.getPanel(api.id)?.params?.view === "reading";
    if (!dock.closeTab(api)) return false;
    if (reading) {
      setEditAlongsideReading((previous) => {
        if (!previous.has(path)) return previous;
        const remaining = new Set(previous);
        remaining.delete(path);
        return remaining;
      });
    }
    if (!simulated && !dock.api?.panels.some((panel) => (panel.params as { path?: string } | undefined)?.path === path)) {
      repository.stopWatching(path);
    }
    return true;
  };
  const dirtyNote = (path: string) => isDirty(simulated ? notes[path] : getNote(path));
  const close = (path: string, api: FilePanelApi) => {
    if (!dirtyNote(path) || !dock.api || keepOnlyCleanClose(dock.api, path, api)) {
      closePanel(path, api);
      return true;
    }
    setDialog({ kind: "close", path, api });
    return false;
  };
  const dockCommands = { dirty: dirtyNote, requestClose: close, save, loadClosedFile: async (path: string) => {
    if (simulated) return notes[path] ? path : undefined;
    const canonical = await loadNote(path);
    if (canonical) watchNote(canonical);
    setGraph(false);
    return canonical;
  } };
  const confirmClose = async ({ path, api }: Extract<KnowledgeDialog, { kind: "close" }>, saveChanges: boolean) => {
    if (confirmingClose) return;
    setConfirmingClose(true);
    try {
      if (saveChanges) {
        if (!await save(path)) return;
      } else await discard(path);
      if (closePanel(path, api)) {
        setDialog(undefined);
        setProblem("");
        dock.resumeCloseBatch(dockCommands);
      }
    } catch (failure) { setProblem(String(failure)); }
    finally { setConfirmingClose(false); }
  };
  const closeDialog = () => {
    if (dialog?.kind === "close" && confirmingClose) return;
    dock.cancelCloseBatch();
    setDialog(undefined);
    setProblem("");
  };

  const reviewExternalChange = (path: string, external: string) => setDialog({ kind: "conflict", path, external });

  const requestRename = async (path: string) => {
    if (simulated) {
      setDestination(path);
      setDialog({ kind: "rename", path });
      return;
    }
    const canonical = await loadNote(path);
    if (!canonical) return;
    const current = getNote(canonical);
    if (!current || isDirty(current) || current.sourceProblem) {
      setProblem("Save the note and resolve external changes before renaming it.");
      return;
    }
    setDialog({ kind: "repositoryRename", path: canonical, revision: current.base.id });
  };

  const readOpenNote = useCallback((path: string) => dock.api?.panels.some((panel) => panel.params?.path === path)
    ? getNote(path)?.content : undefined, [dock.api, getNote]);
  useCheckoutTextReader(checkout.checkoutId, !simulated, readOpenNote);
  const currentText = (path: string) => simulated ? examples[path]?.content : readOpenNote(path);
  const menu = fileContextActions({ checkout, simulated, status: setStatus, report: setProblem, currentText });
  const filePreview = useCheckoutFilePreview({ checkoutId: checkout.checkoutId, simulated, currentText,
    open: (path) => open(path, undefined, undefined, "source"),
  });
  const revealNote = (path: string) => { setSidebar(true); setDisplay("tree"); setReveal({ path }); };
  const comparison = useCheckoutFileComparison({ checkoutId: checkout.checkoutId, simulated, bufferAt: (path) => notes[path],
    openHistory: (path) => openWorkspace("git", fileHistoryTarget(path)),
    restoreLocalText: async (path, content) => {
      const canonical = await loadNote(path);
      if (canonical !== path) throw new Error("This note now resolves to a different path.");
      await repository.loadLocalRevision(path, content);
      watchNote(path);
      setGraph(false);
      setMode("source");
      dock.open(path);
      setStatus(`Loaded a local revision of ${path} into the editor`);
    } });
  async function openSelected(paths: readonly string[]) {
    const request = ++lastOpen.current;
    const loaded = simulated ? paths : await Promise.all(paths.map(loadNote));
    if (request !== lastOpen.current) return;
    setGraph(false);
    for (const path of loaded) {
      if (path) { if (!simulated) watchNote(path); dock.open(path); }
    }
  }
  const startNotesFromTarget = useEffectEvent((paths: string[]) => { void openSelected(paths); });
  useEffect(() => {
    const paths = readOpenNotesTarget(target)?.flatMap((path) => normalizeRepositoryPath(path) ?? []);
    if (paths?.length && dock.ready) startNotesFromTarget(paths);
  }, [dock.ready, target]);
  const treeActions = useCheckoutTreeActions({ checkoutId: checkout.checkoutId, simulated, knowledge: true, report: setProblem,
    refresh: () => Promise.all([repository.refreshCatalog(), noteIndex.index.refetch({ throwOnError: true })]),
    onCreated: (path, kind, copiedFromDisk) => {
      setStatus(copiedFromDisk ? `Copied ${path}` : `Created ${path}`);
      if (kind === "folder" || copiedFromDisk) setReveal({ path });
      else open(path);
    },
  });
  const conversationActions = useCheckoutConversationActions(checkout.checkoutId, simulated, (id) => openWorkspace("conversations", id));
  const startRenameFromTarget = useEffectEvent((path: string) => { void requestRename(path); });
  useEffect(() => {
    const rename = readNoteRenameTarget(target);
    if (rename && dock.ready) startRenameFromTarget(rename);
  }, [dock.ready, target]);
  const fileOperations = useCheckoutItemOperations({ checkoutId: checkout.checkoutId, simulated,
    folders: treeActions.folders, refreshTree: treeActions.refreshTree, report: setProblem, status: setStatus,
    isDirty: (items) => Object.keys(notes).some((path) => isDirty(notes[path]) && items.some((item) => itemIncludesPath(item, path))),
    onRenameNote: (path) => { void requestRename(path); },
    onReveal: (path) => { setSidebar(true); setDisplay("tree"); setReveal({ path }); },
    onChanged: (edits) => {
      const next = dock.reconcilePaths((path) => isDirty(notes[path]) ? undefined : changedItemPath(path, edits));
      for (const path of Object.keys(notes)) if (!isDirty(notes[path]) && changedItemPath(path, edits) !== undefined) repository.forget(path);
      for (const path of next) void loadNote(path).then((canonical) => { if (canonical) watchNote(canonical); });
    },
  });
  const openNoteFavorite = (favorite: CheckoutNoteFavorite) => {
    const navigate = (path: string | undefined) => {
      const content = path ? (simulated ? examples[path] : getNote(path))?.content : undefined;
      const line = content === undefined ? undefined : favoriteAnchorLine(content, favorite.kind, favorite.anchor, favorite.occurrence);
      if (!path || line === undefined) {
        setProblem("The saved " + favorite.kind + " was not found in " + favorite.path + ".");
        return;
      }
      setProblem("");
      open(path, line);
    };
    if (simulated) { navigate(favorite.path); return; }
    const request = ++lastOpen.current;
    void loadNote(favorite.path).then((path) => { if (request === lastOpen.current) navigate(path); });
  };

  const startFromSavedNote = (path: string, kind: "plan" | "launch") => {
    void loadNote(path).then((canonical) => {
      const selected = canonical ? getNote(canonical) : undefined;
      if (selected && !selected.sourceProblem && !isDirty(selected)) setDialog({ kind, source: selected.base });
      else setProblem("Save the note and resolve external changes before using its revision.");
    });
  };
  const mergeActions = (source: string, options?: { destination?: string; ownerPanel?: FilePanelApi }): MenuGroups[number] => {
    const { destination, ownerPanel } = options ?? {};
    return !simulated && catalog.some((note) => note.path !== source) ? [{
      label: destination ? "Merge selected notes…" : "Merge into another note…",
      disabled: isDirty(notes[source]) || Boolean(repository.notes[source]?.sourceProblem)
        || Boolean(destination && (isDirty(notes[destination]) || repository.notes[destination]?.sourceProblem)),
      run: () => setDialog({ kind: "merge", source, destination, ownerPanel }),
    }] : [];
  };
  const noteMenu = (path: string, ownerPanel?: FilePanelApi) => {
    const external = notes[path]?.external;
    return [[
      { label: "Open", run: () => open(path) },
      { label: "Open to the side", run: () => open(path, undefined, undefined, "side") },
      ...dock.openGroupActions(path, (groupId) => open(path, undefined, undefined, { groupId })),
      { label: "Open in new window", run: () => open(path, undefined, undefined, "window") },
      { label: "Open as", items: [
        { label: "Source", run: () => open(path, undefined, undefined, "source") },
        { label: "Live Preview", run: () => open(path, undefined, undefined, "live") },
        { label: "Reading", run: () => open(path, undefined, undefined, "reading") },
      ] },
    ], [
      { label: "Rename note…", disabled: !simulated && isDirty(notes[path]), run: () => { void requestRename(path); } },
      ...mergeActions(path, { ownerPanel }),
      ...(!simulated ? [{ label: "Save a copy…", run: () => treeActions.saveCopy(path, dirtyNote(path) ? currentText(path) : undefined) }] : []),
      { label: "Copy wikilink", run: () => menu.copy(`[[${path}]]`, "Copied wikilink") },
      { label: "Show backlinks and outgoing links", run: () => { setInspector("links"); open(path); } },
      ...(!simulated ? [{ label: "Open working diff", run: () => openWorkspace("git", workingDiffTarget(path)) }] : []),
      ...(external === undefined ? [] : [{ label: "Review external change", run: () => reviewExternalChange(path, external) }]),
    ], [
      ...(!simulated ? [
        { label: "Plan from note…", disabled: isDirty(notes[path]), run: () => startFromSavedNote(path, "plan") },
        { label: "Run workflow from note…", disabled: isDirty(notes[path]), run: () => startFromSavedNote(path, "launch") },
      ] : []),
    ]];
  };

  const navigateLocation = ({ path, anchor }: NoteLocation) => {
    const targetSource = notes[path]?.content ?? "";
    open(path, anchor ? anchorLine(targetSource, anchor) : undefined, anchor);
  };
  const openLocation = ({ path, line, anchor }: KnowledgeLocation) => open(path, line ?? undefined, anchor ?? undefined);
  const anchorFavoriteActions = (path: string, label: string, kind: NoteAnchorKind, line?: number): MenuGroups[number] => {
    const content = (simulated ? examples[path] : getNote(path))?.content;
    const identity = content === undefined ? undefined : noteAnchorIdentity(content, kind, label, line);
    if (!identity) return [];
    const favorite = { kind, path, label, ...identity };
    const saved = favoriteKeys.has(checkoutFavoriteKey(favorite));
    return [{
      label: saved ? "Remove " + kind + " from favorites" : "Add " + kind + " to favorites",
      run: () => setCheckoutFavorites(favoritesId, [favorite], saved),
    }];
  };
  const locationActions = ({ path, line, anchor }: KnowledgeLocation, kind?: NoteAnchorKind) => [
    ...(anchor ? anchorFavoriteActions(path, anchor, kind ?? (anchor.startsWith("^") ? "block" : "heading"), line ?? undefined) : []),
    { label: "Open to the side", run: () => open(path, line ?? undefined, anchor ?? undefined, "side") },
    { label: "Reveal in note tree", run: () => { setSidebar(true); setDisplay("tree"); setReveal({ path }); } },
    ...(!/[#\[\]|]/.test(path) && (!anchor || !/[\[\]|]/.test(anchor)) ? [
      { label: "Copy wikilink", run: () => menu.copy(`[[${path}${anchor ? `#${anchor}` : ""}]]`, "Copied wikilink") },
    ] : []),
    { label: "Copy location", run: () => menu.copy(`${path}${anchor ? `#${anchor}` : ""}${line ? `:${line}` : ""}`, "Copied note location") },
    ...(!simulated ? [{ label: "Copy OmniAgent link", run: () => menu.copyOmniAgentLink(path, line ?? undefined) }] : []),
  ];
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
      const markdown = simulated ? repairWikiAddress(current.content, proposal.link.raw, selection, anchor, proposal.sourceLine)
        : await invoke<string>("repair_note_wikilink", {
          markdown: current.content, reference: proposal.link.raw, sourceLine: proposal.sourceLine ?? null,
          destination: selection, anchor: anchor ?? null,
          ...(proposal.link.syntax === "markdown" ? { markdownSourcePath: proposal.from } : {}),
          ...(proposal.link.sourceOffset !== undefined ? { sourceOffset: proposal.link.sourceOffset } : {}),
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
  const navigateLink = async (from: string, link: NoteTarget, action: "open" | "side" | "repair" = "open") => {
    from = link.sourcePath ?? from;
    const attachment = simulated ? noteAttachments.find(({ name }) => link.target === name) : undefined;
    if (attachment) { setDialog({ kind: "attachment", attachment }); return; }
    try {
      if (!simulated) {
        if (!noteIndex.index.data) await noteIndex.index.refetch();
      }
      const result = simulated ? resolveExampleLink(link, from, catalog) : await resolveRepositoryLink(checkout.checkoutId, link, from);
      if (result?.status === "resolved" && action !== "repair") {
        const { path, line, anchor } = result.location;
        open(path, line ?? undefined, anchor ?? undefined, action === "side" ? "side" : undefined);
      } else if (result?.status === "attachment") {
        setDialog({ kind: "repositoryAttachment", paths: result.paths, label: link.alias ?? link.target });
      } else {
        const targets = result?.status === "ambiguous" ? result.candidates.map(asNoteLocation)
          : result?.status === "missing_anchor" || result?.status === "resolved" ? [asNoteLocation(result.location)] : [];
        if (!simulated && targets.length === 1) void loadNote(targets[0]!.path);
        offerLinkRepair(from, link, targets, targets.length === 1 ? link.anchor : undefined, link.sourceLine,
          link.syntax === "markdown" && (simulated || !link.editableInline));
      }
    } catch (failure) {
      setProblem(String(failure));
    }
  };

  const attachmentActions = (paths: readonly string[], label: string): MenuGroups => {
    const path = paths.length === 1 ? paths[0] : undefined;
    return [[
      { label: "Preview attachment", run: () => setDialog({ kind: "repositoryAttachment", paths: [...paths], label }) },
      ...(path ? [{ label: "Save a copy", run: () => {
        setProblem("");
        void readExactRepositoryAttachment(checkout.checkoutId, path).then((file) => {
          if (!file) throw new Error("Attachment unavailable.");
          downloadRepositoryAttachment(file);
        }).catch((failure: unknown) => setProblem(String(failure)));
      } }] : []),
    ],
      ...(path ? [...menu.location({ kind: "file", path }), ...conversationActions.actions({ kind: "file", path })]
        : [[{ label: "Copy candidate paths", run: () => menu.copy(paths.join("\n"), "Copied attachment paths") }]])];
  };

  const searchForTag = (tag: string) => {
    setQuery("#" + tag); setSidebar(true); setDisplay("search"); requestSearchFocus((before) => before + 1);
  };
  const tagActions = (tag: string, ownerPanel?: FilePanelApi): MenuGroups[number] => [
    { label: "Copy tag", run: () => menu.copy("#" + tag, "Copied tag") },
    { label: "Search notes for tag", run: () => searchForTag(tag) },
    ...(!simulated ? [{
      label: "Rename tag in checkout…", run: () => setDialog({ kind: "tagRename", tag, ownerPanel }),
    }] : []),
  ];
  const noteContextActions = ({ path, line, anchor, anchorKind, anchorLine: hitAnchorLine, link, attachment, tag, excerpt, selection: noteSelection, property, mermaid, diagramImage }: NoteContextHit, reading: boolean, ownerPanel?: FilePanelApi): MenuGroups => {
    const groups: MenuGroups[number][] = [];
    if (attachment && !simulated) groups.push(...attachmentActions(attachment.paths, attachment.label));
    if (link) {
      const from = link.sourcePath ?? path;
      const resolved = simulated ? resolveExampleLink(link, from, catalog) : lookupLink(link, from);
      const target = resolved?.status === "resolved" ? noteWikilink(resolved.location.path, resolved.location.anchor ?? undefined) : undefined;
      const manual = link.syntax === "markdown" && (simulated || !link.editableInline);
      const editSource = () => open(from, link.sourceLine, undefined, "source");
      if (resolved?.status === "attachment" && !simulated) groups.push(...attachmentActions(resolved.paths, link.alias ?? link.target));
      else groups.push([
        { label: "Open link", run: () => { void navigateLink(from, link); } },
        ...(resolved?.status !== "attachment" ? [{ label: "Open link to the side", run: () => { void navigateLink(from, link, "side"); } }] : []),
        ...(resolved?.status !== "attachment" ? [{ label: manual && link.sourceLine ? "Edit Markdown link in source" : resolved?.status === "resolved" ? "Change link target…" : "Resolve note link…",
          run: () => {
            if (manual && link.sourceLine) editSource();
            else void navigateLink(from, link, "repair");
          } }] : []),
        ...(!manual && link.sourceLine ? [{ label: "Go to link in source", run: editSource }] : []),
        ...(target ? [{ label: "Copy target wikilink", run: () => menu.copy(target, "Copied target wikilink") }] : []),
        { label: "Copy link address", run: () => menu.copy(link.raw, "Copied link address") },
      ]);
    }
    if (anchor) {
      const reference = noteWikilink(path, anchor);
      const block = notes[path] && noteAnchorExcerpt(notes[path].content, anchor);
      const kind = anchorKind ?? (anchor.startsWith("^") ? "block" : "heading");
      groups.push([
        ...anchorFavoriteActions(path, anchor, kind, hitAnchorLine ?? line),
        ...(reference ? [{ label: `Copy ${kind} wikilink`,
          run: () => menu.copy(reference, "Copied wikilink") }] : []),
        { label: "Open reference to the side", run: () => open(path, line, anchor, "side") },
        ...(block ? [
          { label: `Copy ${kind} Markdown`, run: () => menu.copy(block.text, "Copied note Markdown") },
          ...(!simulated ? [{ label: `Add ${kind} to conversation…`, run: () => conversationActions.startExcerpt({ path, ...block }) }] : []),
        ] : []),
      ]);
    }
    if (reading && line) groups.push([{ label: "Go to source", run: () => open(path, line, anchor, "source") }]);
    if (property) groups.push([
      { label: "Copy property name", run: () => menu.copy(property.name, "Copied property name") },
      { label: "Copy property value", run: () => menu.copy(property.value, "Copied property value") },
    ]);
    if (mermaid) groups.push([
      { label: "Copy Mermaid source", run: () => menu.copy(mermaid, "Copied Mermaid source") },
      ...(diagramImage ? [{ label: "Download diagram as PNG", run: () => {
        const name = fileName(path).replace(/\.(?:md|markdown)$/i, "") + "-diagram" + (line ? "-" + line : "") + ".png";
        void downloadNoteDiagramPng(diagramImage, name).catch((failure: unknown) => setProblem(String(failure)));
      } }] : []),
    ]);
    if (tag) groups.push(tagActions(tag, ownerPanel));
    const selectionSource = repository.notes[path];
    if (excerpt) groups.push([
      { label: "Copy selection", run: () => menu.copy(excerpt.text, "Copied note selection") },
      { label: "Copy selection reference", run: () => menu.copy(`${excerpt.path}:${excerpt.startLine}${excerpt.endLine === excerpt.startLine ? "" : `-${excerpt.endLine}`}`, "Copied note selection reference") },
      ...(!simulated && excerpt.text.trim() ? [{ label: "Add selection to conversation…", run: () => conversationActions.startExcerpt(excerpt) }] : []),
      ...(!simulated && !reading && selectionSource?.fileIdentity
        && !selectionSource.incoming && !selectionSource.sourceProblem && noteSelection?.text.trim() ? [{
        label: "Extract selection to a new note…", run: () => setDialog({ kind: "extract", path, selection: noteSelection, ownerPanel }),
      }] : []),
    ]);
    const startLine = excerpt?.path === path ? excerpt.startLine : anchor ? line : undefined;
    const endLine = excerpt?.path === path ? excerpt.endLine : startLine;
    if (!simulated && startLine && endLine && notes[path]) {
      const lines = notes[path].content.replace(/\r\n?/g, "\n").split("\n");
      if (endLine >= startLine && endLine <= lines.length) groups.push([
        { label: "Copy OmniAgent link to line", run: () => menu.copyOmniAgentLink(path, startLine) },
        { label: "Copy committed line permalink", run: () => menu.copyPermalink(path, {
          startLine, endLine, text: lines.slice(startLine - 1, endLine).join("\n"),
        }) },
      ]);
    }
    return groups;
  };

  const graphNodeActions = (path: string): MenuGroups => [locationActions({ path }), [
    { label: "Show note links", run: () => { setInspector("links"); open(path); } },
    ...(!simulated ? [{ label: "Show file history", run: () => openWorkspace("git", fileHistoryTarget(path)) }] : []),
  ]];

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
    reconcileFavoriteItems("example", (path) => path === proposal.path ? proposal.destination : undefined);
    for (const panel of [...dock.api?.panels ?? []]) {
      if (panel.params?.path === proposal.path) closePanel(proposal.path, panel.api);
    }
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
          {display === "search" ? <KnowledgeSearch query={query} onQueryChange={setQuery} results={matches}
            pending={!simulated && noteIndex.searching} error={!simulated && noteIndex.error ? String(noteIndex.error) : undefined}
            focusRequest={focusSearch} onOpen={openLocation} />
            : <FileTree paths={catalog.map(({ path }) => path)} folders={treeActions.folders} active={active} onOpen={open} onOpenNoteFavorite={openNoteFavorite} checkoutId={checkout.checkoutId} workspace="knowledge" simulated={simulated} reveal={reveal} actions={(item, selection) => selection ? [
              [
                { label: "Open selected notes", disabled: !selection.some(({ kind }) => kind === "file"),
                  run: () => { void openSelected(selection.filter(({ kind }) => kind === "file").map(({ path }) => path)); } },
                ...comparison.selectionActions(selection),
                ...(selection.length === 2 && selection.every(({ kind }) => kind === "file")
                  ? mergeActions(item.path, { destination: selection.find(({ path }) => path !== item.path)?.path }) : []),
                { label: "Copy relative paths", run: () => menu.copy(selection.map(({ path }) => path).join("\n"), "Copied relative paths") },
              ],
              ...conversationActions.actions(item, selection), ...fileOperations.actions(item, selection),
            ] : [
              ...treeActions.actions(item),
              ...(item.kind === "file" ? [[filePreview.action(item.path)], ...noteMenu(item.path)] : []),
              ...(item.kind !== "file" ? [[{ label: "Find in folder", run: () => openWorkspace("ide", folderSearchTarget(item.path)) }]] : []),
              ...(!simulated ? [[{ label: "Open terminal here", run: () => openWorkspace("ide", terminalFolderTarget(item.kind === "file" ? containingFolder(item.path) : item.path)) }]] : []),
              ...menu.location(item),
              ...(item.kind === "file" ? [comparison.actions(item.path)] : []),
              ...conversationActions.actions(item),
              ...fileOperations.actions(item),
            ]} />}
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
            {([["source", "Source"], ["live", "Live Preview"], ["reading", "Reading"]] as const).map(([choice, label]) => (
              <button key={choice} type="button" className="fw-btn" disabled={!note} aria-pressed={activeMode === choice} onClick={() => {
                if (active) {
                  dock.open(active);
                  setMode(choice);
                  setEditAlongsideReading(new Set());
                }
              }}>{label}</button>
            ))}
          </div>
          <div className="file-toolbar">
            <button type="button" className="fw-icon" title="Save note (Ctrl+S)" aria-label="Save note" disabled={!isDirty(note)}
              onClick={() => { if (active) save(active); }}><Save size={15} /></button>
            <button type="button" className="fw-icon" title="Split Reading view" aria-label="Split Reading view" disabled={!note}
              onClick={() => {
                if (!active) return;
                if (mode === "reading") setEditAlongsideReading((before) => new Set(before).add(active));
                dock.split(active, "reading");
              }}><SplitSquareHorizontal size={15} /></button>
            <button type="button" className="fw-icon" title="Rename note" aria-label="Rename note"
              disabled={!note || (!simulated && (isDirty(note) || Boolean(active && repository.notes[active]?.sourceProblem)))}
              onClick={() => { if (active) void requestRename(active); }}><FilePenLine size={15} /></button>
          </div>
        </div>}
        <div className="note-body">
          <div className="file-dock-body" hidden={graph}>
            <FileDock onReady={dock.onReady} context={{
              renderFile: (path, view, panelId) => {
                const entry = notes[path];
                if (!entry) {
                  const failure = repository.failures[path];
                  return failure || simulated
                    ? <FileUnavailable title="Note unavailable" path={path} detail={failure} />
                    : <p role="status" className="file-help">Loading {path}…</p>;
                }
                const selectedMode = view === "reading" ? "reading" : editorMode(path);
                const ownerPanel = dock.api?.getPanel(panelId)?.api;
                const onLink = (link: NoteTarget) => { void navigateLink(path, link); };
                const sourceProblem = repository.notes[path]?.sourceProblem;
                return (
                  <div className="note-file-view">
                    <FileCaption path={path} revision={entry.revision} onReveal={revealNote} actions={(item) => [
                      [{ label: "Reveal in note tree", run: () => revealNote(item.path) },
                        ...(item.kind === "folder" ? [{ label: "Find in folder", run: () => openWorkspace("ide", folderSearchTarget(item.path)) }] : mergeActions(item.path, { ownerPanel })),
                        ...(!simulated ? [{ label: "Open terminal here", run: () => openWorkspace("ide", terminalFolderTarget(item.kind === "file" ? containingFolder(item.path) : item.path)) }] : [])],
                      ...menu.location(item),
                    ]} />
                    {sourceProblem && <p role="alert" className="file-help">{sourceProblem}</p>}
                    {!simulated && path !== active && entry.external !== undefined && <button type="button" className="fw-btn fw-outlined"
                      onClick={() => reviewExternalChange(path, entry.external!)}>Review external change</button>}
                    <Suspense fallback={<p role="status" className="file-help">Loading note view…</p>}>
                      {view === "editor" && <div className="note-editable-view" hidden={selectedMode === "reading"}>
                        <Editor mode={selectedMode === "source" ? "source" : "live"} path={path} source={entry.content} files={catalog} jump={jumps[path]} analyze
                          suggestLinks={simulated ? undefined : (prefix) => suggestRepositoryLinks(checkout.checkoutId, path, prefix)}
                          onChange={(content) => update(path, content)} onLink={onLink}
                          contextActions={(hit) => noteContextActions(hit, false, ownerPanel)} />
                      </div>}
                      {selectedMode === "reading" && <Reading path={path} content={entry.content} files={catalog} jump={jumps[path]} onLink={onLink}
                        checkoutId={simulated ? undefined : checkout.checkoutId} sampleAttachments={simulated}
                        resolveLinks={simulated ? undefined : lookupLink}
                        previewFor={simulated ? undefined : previewNote} contextActions={(hit) => noteContextActions(hit, true, ownerPanel)}
                        onAttachment={(attachment) => setDialog({ kind: "attachment", attachment })}
                        onRepositoryAttachment={(paths, label) => setDialog({ kind: "repositoryAttachment", paths, label })} />}
                    </Suspense>
                  </div>
                );
              },
              empty: <FileUnavailable title="Open a note"
                detail="Choose a note from the tree or search by title and content."
                onSimulate={simulated ? undefined : () => startSimulation()} />,
              dirty: dirtyNote,
              requestClose: close,
              layoutActions: (api) => dock.layoutActions(api, dockCommands),
              groupActions: (id) => dock.groupActions(id, dockCommands),
              tabActions: (path, view, api) => [[
                { label: "Save", disabled: !isDirty(notes[path]), run: () => { void save(path); } },
                { label: "Split right", run: () => dock.split(path, view, "right", api.id) },
                { label: "Split down", run: () => dock.split(path, view, "below", api.id) },
                { label: "Reveal in note tree", run: () => revealNote(path) },
              ], ...noteMenu(path, api).slice(1), ...menu.location({ kind: "file", path }), comparison.actions(path, api), ...conversationActions.actions({ kind: "file", path }), ...fileOperations.actions({ kind: "file", path }), [
                { label: "Close tab", run: () => close(path, api) },
              ]],
            }} />
          </div>
          {graph && <div className="note-graph-host">
            {!simulated && (graphEdges.error || noteIndex.index.error) ? <p role="alert" className="file-help">{String(graphEdges.error ?? noteIndex.index.error)}</p>
              : <Suspense fallback={<p role="status" className="file-help">Loading note graph…</p>}>
                {simulated ? <NoteGraph files={catalog} selected={active} onOpen={open} onNodeActions={graphNodeActions} />
                  : noteIndex.index.data && graphEdges.data ? <NoteGraph projection={{ notes: noteIndex.index.data, edges: graphEdges.data }} selected={active} onOpen={open} onNodeActions={graphNodeActions} />
                    : <p role="status" className="file-help">Loading note graph…</p>}
              </Suspense>}
          </div>}
          {inspector && !graph && <ResizablePanel as="aside" className="note-inspector" aria-label="Note details"
            edge="left" resizeKey="note-details" resizeLabel="note details">
            <div className="note-inspector-views">
              {([[ListTree, "outline", "Outline"], [Link2, "links", "Links"], [FileText, "properties", "Properties"], [Paperclip, "attachments", "Attachments"]] as const)
                .map(([Icon, view, label]) => <button type="button" key={view} className="fw-icon" aria-label={label} aria-pressed={view === inspector} title={label}
                  onClick={() => setInspector(view)}><Icon size={16} /></button>)}
            </div>
            <div className="note-inspector-content">
              {!active || !note ? <p className="file-help">Open a note to see its details.</p> : <>
                {inspector === "outline" && <KnowledgeOutline
                  entries={(isDirty(note) ? headings(source) : outlineIndex.data?.headings ?? headings(source))
                    .map((entry) => ({ ...entry, path: active, anchor: entry.label }))} onOpen={openLocation} actions={(location) => locationActions(location, "heading")} />}
                {inspector === "links" && <KnowledgeLinks outgoing={outgoingLinks} backlinks={backlinks}
                  onOpen={openLocation} actions={locationActions} copyReference={(text) => menu.copy(text, "Copied link target")}
                  pending={showIndexed && relationships.isPending}
                  error={relationships.error ? String(relationships.error) : undefined}
                  onRepair={(entry) => {
                    const resolution = entry.resolution;
                    const link = entry.reference;
                    if (!link) return;
                    const targets = resolution.kind === "ambiguous" ? resolution.candidates : resolution.kind === "missing_anchor" ? [resolution.target] : [];
                    if (!simulated && targets.length === 1) void loadNote(targets[0]!.path);
                    offerLinkRepair(active, { ...link, raw: entry.rawTarget }, targets.map((target) => ({ path: target.path, anchor: target.anchor ?? undefined })),
                      resolution.kind === "missing_anchor" ? link.anchor : undefined, entry.source.line ?? undefined, !link.wiki);
                  }} />}
                {inspector === "properties" && <section aria-label="Properties"><h3>Properties</h3>
                  {getProperties(source).map(({ key, value, line }, index) =>
                    <NotePropertyEditor key={`${active}:${key}:${index}`} name={key} value={value}
                      read={() => simulated ? note?.content : getNote(active)?.content}
                      replace={simulated ? updateProperty : undefined} update={(content) => update(active, content)} actions={[
                        { label: "Go to property in source", run: () => open(active, line, undefined, "source") },
                        { label: "Copy property name", run: () => menu.copy(key, "Copied property name") },
                        { label: "Copy property value", run: () => menu.copy(value, "Copied property value") },
                      ]} />)}
                  {getProperties(source).length === 0 && <p className="file-help">This note has no frontmatter properties.</p>}
                  {!isDirty(note) && previewNote(active)?.tags?.length ? <ul aria-label="Tags in this note" className="flex flex-wrap gap-1 pt-2">
                    {previewNote(active)?.tags.map((tag) => <li key={tag}>
                      <ContextActions label={"#" + tag} groups={[tagActions(tag)]}>
                        <button type="button" className="fw-btn" onClick={() => searchForTag(tag)}>#{tag}</button>
                      </ContextActions>
                    </li>)}
                  </ul> : null}
                </section>}
                {inspector === "attachments" && <section aria-label="Attachments"><h3>Attachments</h3>
                  {simulated && noteAttachments.map((attachment) => <button type="button" className="note-attachment-row" key={attachment.name} onClick={() => setDialog({ kind: "attachment", attachment })}>
                    <strong>{attachment.name}</strong><small>{attachment.detail}</small>
                  </button>)}
                  {!simulated && outgoing.flatMap((link, index) => {
                    const result = lookupLink(link, active);
                    const label = link.alias ?? link.target;
                    return result?.status === "attachment" ? [<ContextActions key={index} label={label} groups={attachmentActions(result.paths, label)}>
                      <button type="button" className="note-attachment-row" onClick={() => navigateLink(active, link)}>{label}</button>
                    </ContextActions>] : [];
                  })}
                </section>}
              </>}
            </div>
          </ResizablePanel>}
        </div>

        {active && note && !graph && <footer className="ide-statusbar note-statusbar" aria-label="Note status and actions">
          <span>{note.external !== undefined ? "External changes" : isDirty(note) ? "Unsaved changes" : savedNote?.sourceProblem ? "Note unavailable" : "Saved"}</span>
          <div className="note-status-actions">
            {note.external !== undefined && <button type="button" className="fw-btn"
              onClick={() => reviewExternalChange(active, note.external!)}>Review external change</button>}
            {simulated && note.external === undefined && <button type="button" className="fw-btn" onClick={() => {
              const example = examples[active];
              if (!example) return;
              const external = example.saved + "\n\n> External review added a new readiness note.\n";
              setNotes((before) => ({ ...before, [active]: { ...example, external } }));
              setStatus(`External changes need review in ${active}`);
              reviewExternalChange(active, external);
            }}>Simulate external change</button>}
            {showPlanAction && <button type="button" className="fw-btn fw-primary" disabled={!canPlan}
              title={canPlan ? "Start Plan Mode from the saved note" : "Save this note without conflicts before starting Plan Mode"}
              onClick={() => {
                if (savedNote) setDialog({ kind: "plan", source: savedNote.base });
                else if (simulation) { simulation.advanceDelivery("start"); openWorkspace("conversations", "plan"); }
              }}><Wand2 size={14} />Plan from note</button>}
            {showLinkedTask && <button type="button" className="fw-btn" onClick={() => {
                if (simulation) openWorkspace("tasks", "release");
                else if (linkedTasks.length === 1) openWorkspace("tasks", linkedTasks[0]?.id);
                else setDialog({ kind: "linkedTasks" });
              }}><GitFork size={14} />{linkedTasks.length > 1 ? "Open linked tasks" : "Open linked task"}</button>}
            {savedNote && <button type="button" className="fw-btn" disabled={!canLaunchFromNote}
              title={canLaunchFromNote ? "Choose a published workflow and review this revision" : "A saved note without conflicts is required"}
              onClick={() => setDialog({ kind: "launch", source: savedNote.base })}><Wand2 size={14} />Run workflow from note</button>}
          </div>
        </footer>}
      </div>

      {dialog?.kind === "plan" && <RepositoryNotePlanning projectId={checkout.projectId} source={dialog.source}
        openWorkspace={openWorkspace} onClose={closeDialog} />}
      {dialog?.kind === "linkedTasks" && <FileDialog title="Linked tasks" description="Choose a Task linked to this saved note revision." onClose={closeDialog}>
        <ul className="pipeline-stack">{linkedTasks.map(({ id, title }) => <li key={id}>
          <button type="button" className="fw-btn" onClick={() => { closeDialog(); openWorkspace("tasks", id); }}>{title}</button>
        </li>)}</ul>
      </FileDialog>}
      {treeActions.dialog}
      {filePreview.dialog}
      {conversationActions.dialog}
      {dialog?.kind === "extract" && <ExtractNoteDialog ownerPanel={dialog.ownerPanel} path={dialog.path} selection={dialog.selection} folders={treeActions.folders}
        onClose={closeDialog} onExtract={(options) => repository.extractSelection(dialog.path, dialog.selection, options)}
        onCreated={(path, replaced) => {
          closeDialog();
          setStatus(replaced ? "Created the new note and added a link to the source draft." : "Created the new note.");
          void treeActions.refreshTree().catch((failure: unknown) => setProblem(String(failure)));
          open(path);
        }} />}
      {dialog?.kind === "merge" && <RepositoryNoteMerge ownerPanel={dialog.ownerPanel} checkoutId={checkout.checkoutId} source={dialog.source}
        suggestedDestination={dialog.destination} destinations={catalog.map((note) => note.path).filter((path) => path !== dialog.source)}
        onClose={closeDialog} onApply={repository.mergeReviewed} onOpen={(path) => { closeDialog(); open(path, undefined, undefined, "source"); }}
        onSaved={() => { void treeActions.refreshTree().catch((failure: unknown) => setProblem(String(failure))); }} />}
      {dialog?.kind === "tagRename" && <RepositoryTagRename checkoutId={checkout.checkoutId} source={dialog.tag} ownerPanel={dialog.ownerPanel}
        onClose={closeDialog} onApply={repository.renameReviewedTags} onOpen={(path) => { closeDialog(); open(path, undefined, undefined, "source"); }}
        onSaved={(tag) => {
          if (query === "#" + dialog.tag) setQuery("#" + tag);
          void treeActions.refreshTree().catch((failure: unknown) => setProblem(String(failure)));
        }} />}
      {fileOperations.dialog}
      {comparison.dialog}
      {dialog?.kind === "launch" && <FileDialog title="Run workflow from note" description={dialog.source.relativePath} onClose={closeDialog}>
        <WorkflowLaunchPicker projectId={checkout.projectId} checkoutId={dialog.source.checkoutId}
          pinnedDocumentRevisionId={dialog.source.id} openWorkspace={(workspace, target) => {
            closeDialog();
            openWorkspace(workspace, target);
          }} />
      </FileDialog>}
      {dialog?.kind === "close" && <FileDialog title="Save note before closing?" description={`${dialog.path} has unsaved changes.`} onClose={closeDialog}
        ownerPanel={dialog.api}>
        <div className="file-dialog-footer">
          <button type="button" className="fw-btn" disabled={confirmingClose} onClick={closeDialog}>Cancel</button>
          <button type="button" className="fw-btn fw-destructive" disabled={confirmingClose} onClick={() => void confirmClose(dialog, false)}>Discard changes</button>
          <button type="button" className="fw-btn fw-primary" disabled={confirmingClose} onClick={() => void confirmClose(dialog, true)}>Save and close</button>
        </div>
        {problem && <p role="alert">{problem}</p>}
      </FileDialog>}
      {dialog?.kind === "conflict" && <FileDialog title="Note revision conflict" description={`Review the local and external versions of ${dialog.path}.`}
        ownerPanel={dialog.ownerPanel} onClose={closeDialog}>
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
          reconcileFavoriteItems(checkout.checkoutId, (path) => path.toLowerCase() === dialog.path.toLowerCase() ? moved : undefined);
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
        description={dialog.link.syntax === "markdown" ? `Resolve ${dialog.link.raw} from ${dialog.from}.` : `Resolve [[${dialog.link.raw}]] from ${dialog.from}.`} onClose={closeDialog}>
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
          {(dialog.manual || dialog.sourceLine !== undefined) && <button type="button" className={`fw-btn ${dialog.manual ? "fw-primary" : "fw-outlined"}`} onClick={() => {
            setMode("source");
            open(dialog.from, dialog.sourceLine);
            closeDialog();
          }}>Edit source link</button>}
          {!dialog.manual && <button type="button" className="fw-btn fw-primary" disabled={!selection || Boolean(selectedAnchor && !anchorLine(notes[selection]?.content ?? "", selectedAnchor.trim()))}
            onClick={() => { void repairReference(dialog); }}>Update link</button>}
        </div>
      </FileDialog>}
      {dialog?.kind === "attachment" && <FileDialog title={dialog.attachment.name} description={dialog.attachment.detail} onClose={closeDialog}>
        <AttachmentPreview attachment={dialog.attachment} />
      </FileDialog>}
      {dialog?.kind === "repositoryAttachment" && <FileDialog title={dialog.label} description="Repository attachment" onClose={closeDialog}>
        <RepositoryNoteMedia checkoutId={checkout.checkoutId} paths={dialog.paths} title={dialog.label} allowSaveCopy />
      </FileDialog>}
    </section>
  );
}
