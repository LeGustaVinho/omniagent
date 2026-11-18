import {
  AlertCircle, BookOpen, BookOpenText, FilePenLine, FileText, Link2,
  ListTree, Network, PanelRight, Paperclip, RotateCw, Save, Search, SplitSquareHorizontal, Wand2, X,
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
import { fileName, normalizeRepositoryPath } from "@/ide-knowledge-data";
import {
  anchorLine, favoriteAnchorLine, headings, noteAnchorExcerpt, noteAnchorIdentity, noteFrontmatter, noteMarkdownLink, parseReadingAddress, readingLocalAddress,
  wikiLinks, type NoteAnchorKind, type NoteLocation, type NoteTarget,
} from "@/knowledge-document";
import { downloadNoteDiagramPng } from "@/knowledge-media";
import { noteWikilink, type NoteContextHit, type NoteSelection } from "@/knowledge-context";
import { ExtractNoteDialog } from "@/knowledge-note-extraction";
import { RepositoryNoteMerge } from "@/repository-note-merge";
import { RepositoryTagRename } from "@/repository-tag-rename";
import type { NoteJump } from "@/knowledge-editor";
import { useRepositoryNotes, type NoteBuffer, type NoteRevision } from "@/repository-note-buffers";
import { downloadRepositoryAttachment, readExactRepositoryAttachment, RepositoryNoteMedia } from "@/repository-note-media";
import { NotePropertyEditor } from "@/note-property-editor";
import { useWorkspaceSize, WorkbenchPanel } from "@/workbench-panel";
import { ControlTooltip } from "@/components/ui/control-tooltip";
import { useKnowledgeProperties, useKnowledgePropertySearch, type NotePropertyFilter, type NoteYamlProperty } from "@/knowledge-properties";
import { KnowledgeLinks, KnowledgeOutline, KnowledgeSearch, type KnowledgeLocation, type KnowledgeOutgoingLink } from "@/knowledge-navigation";
import { useRepositoryNoteIndex } from "@/repository-note-index";
import { RepositoryNoteRename } from "@/repository-note-rename";
import { WorkflowLaunchPicker } from "@/workflow-launch";
import { workflowComparisonNoteTarget } from "@/workflow-comparison-note-target";
import { asNoteLocation, linkPreviewValue, resolveRepositoryLink, suggestRepositoryLinks, useRepositoryLinkPreviews,
  type IndexedLinkLocation, type IndexedLinkResolution, type IndexedNoteDetails, type IndexedNoteRelationships } from "@/repository-note-links";
import type { NoteGraphProjection } from "@/knowledge-graph";
import type { WorkspaceProps } from "@/workbench-contract";

const Editor = lazy(() => import("@/knowledge-editor").then(({ MarkdownEditor }) => ({ default: MarkdownEditor })));
const Reading = lazy(() => import("@/knowledge-reading").then(({ NoteReading }) => ({ default: NoteReading })));
const NoteGraph = lazy(() => import("@/knowledge-graph"));

type NoteMode = "source" | "live" | "reading";
type Inspector = "outline" | "links" | "properties" | "attachments";
type NotePresentation = NoteMode | "side" | "window" | { groupId: string } | { readingBeside: string } | { inspector: Inspector };

type KnowledgeDialog =
  | { kind: "close"; path: string; api: FilePanelApi }
  | { kind: "extract"; path: string; selection: NoteSelection; ownerPanel?: FilePanelApi }
  | { kind: "merge"; source: string; destination?: string; ownerPanel?: FilePanelApi }
  | { kind: "tagRename"; tag: string; ownerPanel?: FilePanelApi }
  | { kind: "property"; path: string; property: NoteYamlProperty; source: string; ownerPanel?: FilePanelApi }
  | { kind: "conflict"; path: string; external: string; ownerPanel?: FilePanelApi }
  | { kind: "repositoryRename"; path: string; revision: string }
  | { kind: "launch"; source: NoteRevision }
  | { kind: "link"; from: string; link: NoteTarget; matches: NoteLocation[]; sourceLine?: number; manual?: boolean }
  | { kind: "repositoryAttachment"; paths: string[]; label: string };

function isDirty(note?: NoteBuffer) {
  return Boolean(note && (note.external !== undefined || note.content !== note.saved));
}

export default function KnowledgeWorkspace(props: WorkspaceProps) {
  return <KnowledgeSurface key={props.checkout.checkoutId} {...props} />;
}

function KnowledgeSurface({ checkout, target, openWorkspace }: WorkspaceProps) {
  const [reveal, setReveal] = useState<{ path: string }>();
  const dock = useFileDock({ checkoutId: checkout.checkoutId, workspace: "knowledge" });
  const repository = useRepositoryNotes(checkout.checkoutId);
  const notes = repository.notes;
  const workspace = useRef<HTMLElement>(null);
  const detailSpace = useRef<HTMLDivElement>(null);
  const sidebarToggle = useRef<HTMLButtonElement>(null);
  const inspectorToggle = useRef<HTMLButtonElement>(null);
  const workspaceWidth = useWorkspaceSize(workspace).width;
  const [sidebar, setSidebar, sidebarDrawer] = useFileSidebar(workspaceWidth);
  const detailsDrawer = useWorkspaceSize(detailSpace).width < 800;
  const [display, setDisplay] = useState<"tree" | "search">("tree");
  const [query, setQuery] = useState("");
  const [propertyFilter, setPropertyFilter] = useState<NotePropertyFilter>();
  const [focusSearch, requestSearchFocus] = useState(0);
  const noteIndex = useRepositoryNoteIndex(checkout.checkoutId, propertyFilter ? "" : query);
  const propertiesOf = useKnowledgeProperties(Object.values(notes).map(({ content }) => content));
  const [graph, setGraph] = useState<false | "global" | { path: string; request: number }>(false);
  const graphEdges = useQuery({
    queryKey: ["repository-note-edges", checkout.checkoutId, noteIndex.index.dataUpdatedAt],
    enabled: graph && noteIndex.index.isSuccess,
    placeholderData: (previous, query) => query?.queryKey[1] === checkout.checkoutId ? previous : undefined,
    queryFn: () => invoke<NoteGraphProjection["edges"]>("repository_note_edges", { checkoutId: checkout.checkoutId }),
  });
  const graphProjection = useMemo(() => noteIndex.index.data && graphEdges.data ? { notes: noteIndex.index.data, edges: graphEdges.data } : undefined,
    [noteIndex.index.data, graphEdges.data]);
  const localGraph = typeof graph === "object" ? graph : undefined;
  const [inspectorOverride, setInspector] = useState<Inspector | null>();
  const [mode, setMode] = useState<NoteMode>("live");
  const [editAlongsideReading, setEditAlongsideReading] = useState<ReadonlySet<string>>(() => new Set());
  const [jumps, setJumps] = useState<Record<string, NoteJump>>({});
  const [dialog, setDialog] = useState<KnowledgeDialog>();
  const [confirmingClose, setConfirmingClose] = useState(false);
  const [status, setStatus] = useState("");
  const [problem, setProblem] = useState("");
  const [selection, setSelection] = useState("");
  const [selectedAnchor, setSelectedAnchor] = useState("");
  const lastTarget = useRef<string>(undefined);
  const lastOpen = useRef(0);
  const { load: loadNote, watch: watchNote, getNote } = repository;
  const favoritesId = checkout.checkoutId;
  const { favorites } = useCheckoutFavorites(favoritesId);
  const favoriteKeys = useMemo(() => new Set(favorites.map(checkoutFavoriteKey)), [favorites]);
  const active = dock.path;
  const note = active ? notes[active] : undefined;
  const inspector = inspectorOverride === undefined
    ? note && workspaceWidth >= 1040 && !detailsDrawer ? "outline" : null
    : inspectorOverride;
  const savedNote = active ? repository.notes[active] : undefined;
  const canLaunchFromNote = Boolean(savedNote && !savedNote.sourceProblem && !isDirty(savedNote));
  const conflictRecovery = dialog?.kind === "conflict" ? repository.notes[dialog.path]?.recovery : undefined;
  const source = note?.content ?? "";
  const activeProperties = propertiesOf(source);
  const editorMode = (path: string) => mode === "reading" && editAlongsideReading.has(path) ? "live" : mode;
  const activeMode = dock.view === "reading" ? "reading" : active ? editorMode(active) : mode;
  const showIndexed = Boolean(active) && noteIndex.index.isSuccess;
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
      if (indexed.has(path) || isDirty(value)) indexed.set(path, value.content);
    }
    return [...indexed].map(([path, content]) => ({ path, content }));
  }, [notes, noteIndex.index.data, repository.catalog, repository.draftPaths]);
  const previews = useRepositoryLinkPreviews(checkout.checkoutId, catalog, noteIndex.index.dataUpdatedAt);
  const lookupLink = useCallback((link: NoteTarget, from: string) => linkPreviewValue(previews.data, link, from), [previews.data]);
  const drafts = useMemo(() => Object.fromEntries(Object.entries(repository.notes).filter(([, value]) => isDirty(value) && !value.sourceProblem)
    .map(([path, { content }]) => [path, noteFrontmatter(content)])), [repository.notes]);
  const propertySearch = useKnowledgePropertySearch({ checkoutId: checkout.checkoutId, filter: propertyFilter, drafts,
    indexVersion: noteIndex.index.dataUpdatedAt });
  const needle = query.trim().toLocaleLowerCase();
  const matches = propertyFilter ? propertySearch.hits.filter(({ path, title }) => !needle || path.toLocaleLowerCase().includes(needle)
    || title?.toLocaleLowerCase().includes(needle))
    : noteIndex.hits;
  const outgoing = wikiLinks(source);
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
    isDirty(note) ? outgoing.map((reference) => describeLink(active, reference, lookupLink(reference, active)))
      : relationships.data?.outgoing.map(({ link, resolution }) => {
        const raw = link.target + (link.anchor ? `#${link.anchor}` : "") + (link.alias ? `|${link.alias}` : "");
        const parsed = link.syntax === "markdown" ? parseReadingAddress(readingLocalAddress(active, link.target)) : undefined;
        const reference = link.syntax === "wiki" ? { raw, target: link.target, alias: link.alias ?? undefined, anchor: link.anchor ?? undefined, embed: link.embed }
          : parsed && { ...parsed, alias: link.alias ?? undefined };
        return reference && describeLink(active, reference, resolution, link.line, link.syntax === "wiki");
      }) ?? []
  ).filter((link): link is KnowledgeOutgoingLink => Boolean(link)) : [];
  const backlinks = (relationships.data?.backlinks ?? []).map((link) => ({
    source: { path: link.path, line: link.line, title: link.title, snippet: previewNote(link.path)?.preview },
    label: link.label ?? undefined,
  }));

  const open = useCallback((path: string, line?: number, anchor?: string, presentation?: NotePresentation) => {
    const focus = (file: string, targetLine?: number) => {
      if (targetLine || anchor) setJumps((before) => ({ ...before, [file]: { line: targetLine, anchor, token: (before[file]?.token ?? 0) + 1 } }));
      setGraph(false);
      if (presentation === "side" || (typeof presentation === "object" && "readingBeside" in presentation)) {
        if (mode === "reading") setEditAlongsideReading((before) => new Set(before).add(file));
        dock.split(file, "reading", "right", typeof presentation === "object" ? presentation.readingBeside : undefined);
      } else if (typeof presentation === "object") {
        if ("groupId" in presentation) dock.open(file, presentation.groupId);
        else { dock.open(file); setInspector(presentation.inspector); }
      } else if (presentation === "window") dock.openInWindow(file);
      else {
        if (presentation) {
          setMode(presentation);
          setEditAlongsideReading(new Set());
        }
        dock.open(file);
      }
    };
    const request = ++lastOpen.current;
    void loadNote(path).then((file) => {
      if (!file || request !== lastOpen.current) return;
      const anchorTarget = anchor ? anchorLine(getNote(file)?.content ?? "", anchor) : undefined;
      focus(file, anchorTarget ?? line);
      watchNote(file);
    });
  }, [dock.open, dock.openInWindow, dock.split, getNote, loadNote, mode, watchNote]);

  function closeNavigation() {
    setSidebar(false);
    requestAnimationFrame(() => sidebarToggle.current?.focus({ preventScroll: true }));
  }

  function closeInspector() {
    setInspector(null);
    requestAnimationFrame(() => inspectorToggle.current?.focus({ preventScroll: true }));
  }

  function chooseNote(path: string, line?: number, anchor?: string) {
    open(path, line, anchor);
    if (sidebarDrawer) closeNavigation();
  }

  useEffect(() => {
    if (!dock.ready || !active) return;
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
  }, [active, dock.api, dock.open, dock.ready, loadNote, watchNote]);

  useEffect(() => {
    if (!active || !note) return;
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
  }, [active, loadNote, lookupLink, note, notes, repository.failures]);

  useEffect(() => {
    if (!target || target === lastTarget.current || !dock.ready || readNoteRenameTarget(target) !== undefined || readOpenNotesTarget(target) !== undefined) return;
    const windowPath = readNoteWindowTarget(target);
    const location = windowPath === undefined ? readNoteSearchTarget(target) : { path: windowPath, line: undefined, side: false };
    const next = location && normalizeRepositoryPath(location.path);
    if (!next || !location) return;
    lastTarget.current = target;
    open(next, location.line, undefined, windowPath !== undefined ? "window" : location.side ? "side" : undefined);
  }, [dock.ready, open, target]);

  const update = repository.update;
  const save = async (path: string, ownerPanel?: FilePanelApi) => {
    const external = getNote(path)?.external;
    if (external !== undefined) {
      dock.cancelCloseBatch();
      setDialog({ kind: "conflict", path, external, ownerPanel });
      return false;
    }
    const saving = `Saving ${path}…`;
    setStatus(saving);
    const saved = await repository.save(path);
    if (saved) setStatus(`Saved ${path}`);
    else {
      setStatus((current) => current === saving ? "" : current);
      const incoming = getNote(path)?.external;
      if (incoming !== undefined) {
        dock.cancelCloseBatch();
        setDialog({ kind: "conflict", path, external: incoming, ownerPanel });
      }
    }
    return saved;
  };
  const discard = (path: string) => repository.resolve(path, false);
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
    if (!dock.api?.panels.some((panel) => (panel.params as { path?: string } | undefined)?.path === path)) {
      repository.stopWatching(path);
    }
    return true;
  };
  const dirtyNote = (path: string) => isDirty(getNote(path));
  const close = (path: string, api: FilePanelApi) => {
    if (!dirtyNote(path) || !dock.api || keepOnlyCleanClose(dock.api, path, api)) {
      closePanel(path, api);
      return true;
    }
    setDialog({ kind: "close", path, api });
    return false;
  };
  const dockCommands = { dirty: dirtyNote, requestClose: close, save, loadClosedFile: async (path: string) => {
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
  useCheckoutTextReader(checkout.checkoutId, readOpenNote);
  const currentText = readOpenNote;
  const menu = fileContextActions({ checkout, status: setStatus, report: setProblem, currentText });
  const filePreview = useCheckoutFilePreview({ checkoutId: checkout.checkoutId, currentText,
    open: (path) => open(path, undefined, undefined, "source"),
  });
  const revealNote = (path: string) => { setSidebar(true); setDisplay("tree"); setReveal({ path }); };
  const comparison = useCheckoutFileComparison({ checkoutId: checkout.checkoutId, bufferAt: (path) => notes[path],
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
    const loaded = await Promise.all(paths.map(loadNote));
    if (request !== lastOpen.current) return;
    setGraph(false);
    for (const path of loaded) {
      if (path) { watchNote(path); dock.open(path); }
    }
  }
  const startNotesFromTarget = useEffectEvent((paths: string[]) => { void openSelected(paths); });
  useEffect(() => {
    const paths = readOpenNotesTarget(target)?.flatMap((path) => normalizeRepositoryPath(path) ?? []);
    if (paths?.length && dock.ready) startNotesFromTarget(paths);
  }, [dock.ready, target]);
  const treeActions = useCheckoutTreeActions({ checkoutId: checkout.checkoutId, knowledge: true, report: setProblem,
    refresh: () => Promise.all([repository.refreshCatalog(), noteIndex.index.refetch({ throwOnError: true })]),
    onCreated: (path, kind, copiedFromDisk) => {
      setStatus(copiedFromDisk ? `Copied ${path}` : `Created ${path}`);
      if (kind === "folder" || copiedFromDisk) setReveal({ path });
      else open(path);
    },
  });
  const conversationActions = useCheckoutConversationActions(checkout.checkoutId, false, (id) => openWorkspace("conversations", id));
  const startRenameFromTarget = useEffectEvent((path: string) => { void requestRename(path); });
  useEffect(() => {
    const rename = readNoteRenameTarget(target);
    if (rename && dock.ready) startRenameFromTarget(rename);
  }, [dock.ready, target]);
  const fileOperations = useCheckoutItemOperations({ checkoutId: checkout.checkoutId,
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
      const content = path ? getNote(path)?.content : undefined;
      const line = content === undefined ? undefined : favoriteAnchorLine(content, favorite.kind, favorite.anchor, favorite.occurrence);
      if (!path || line === undefined) {
        setProblem("The saved " + favorite.kind + " was not found in " + favorite.path + ".");
        return;
      }
      setProblem("");
      chooseNote(path, line);
    };
    const request = ++lastOpen.current;
    void loadNote(favorite.path).then((path) => { if (request === lastOpen.current) navigate(path); });
  };

  const openSavedNoteAction = (path: string, kind: "launch" | "compare") => {
    const request = ++lastOpen.current;
    void loadNote(path).then((canonical) => {
      if (request !== lastOpen.current) return;
      const selected = canonical ? getNote(canonical) : undefined;
      const browseSaved = kind === "compare";
      if (!selected || selected.sourceProblem || (!browseSaved && isDirty(selected))) {
        setProblem(browseSaved ? "The saved note revision is unavailable."
          : "Save the note and resolve external changes before using its revision.");
        return;
      }
      if (kind === "compare") {
        const { id, relativePath, sha256, checkoutId } = selected.base;
        openWorkspace("pipelines", workflowComparisonNoteTarget({ checkoutId, revisionId: id, relativePath, sha256 }));
      } else setDialog({ kind, source: selected.base });
    });
  };
  const mergeActions = (source: string, options?: { destination?: string; ownerPanel?: FilePanelApi }): MenuGroups[number] => {
    const { destination, ownerPanel } = options ?? {};
    return catalog.some((note) => note.path !== source) ? [{
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
      { label: "Open Reading to the side", run: () => open(path, undefined, undefined, "side") },
      ...dock.openGroupActions(path, (groupId) => open(path, undefined, undefined, { groupId })),
      { label: "Open in new window", run: () => open(path, undefined, undefined, "window") },
      { label: "Open as", items: [
        { label: "Source", run: () => open(path, undefined, undefined, "source") },
        { label: "Live Preview", run: () => open(path, undefined, undefined, "live") },
        { label: "Reading", run: () => open(path, undefined, undefined, "reading") },
      ] },
    ], [
      { label: "Rename note…", disabled: isDirty(notes[path]), run: () => { void requestRename(path); } },
      ...mergeActions(path, { ownerPanel }),
      { label: "Save a copy…", run: () => treeActions.saveCopy(path, dirtyNote(path) ? currentText(path) : undefined) },
      { label: "Copy wikilink", run: () => menu.copy(`[[${path}]]`, "Copied wikilink") },
      { label: "Copy Markdown link", run: () => menu.copy(noteMarkdownLink(path), "Copied Markdown link") },
      { label: "Show Outline", run: () => open(path, undefined, undefined, { inspector: "outline" }) },
      { label: "Show backlinks and outgoing links", run: () => open(path, undefined, undefined, { inspector: "links" }) },
      { label: "Open local graph", run: () => setGraph({ path, request: ++lastOpen.current }) },
      { label: "Open working diff", run: () => openWorkspace("git", workingDiffTarget(path)) },
      ...(external === undefined ? [] : [{ label: "Review external change", run: () => reviewExternalChange(path, external) }]),
    ], [
      { label: "Run workflow from note…", disabled: isDirty(notes[path]), run: () => openSavedNoteAction(path, "launch") },
      { label: "Compare models from note…", disabled: Boolean(repository.notes[path]?.sourceProblem),
          run: () => openSavedNoteAction(path, "compare") },
    ]];
  };

  const navigateLocation = ({ path, anchor }: NoteLocation) => {
    const targetSource = notes[path]?.content ?? "";
    open(path, anchor ? anchorLine(targetSource, anchor) : undefined, anchor);
  };
  const openLocation = ({ path, line, anchor }: KnowledgeLocation) => open(path, line ?? undefined, anchor ?? undefined);
  const anchorFavoriteActions = (path: string, label: string, kind: NoteAnchorKind, line?: number): MenuGroups[number] => {
    const content = getNote(path)?.content;
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
    { label: "Copy OmniAgent link", run: () => menu.copyOmniAgentLink(path, line ?? undefined) },
  ];
  const offerLinkRepair = (from: string, link: NoteTarget, matches: NoteLocation[], anchor = "", sourceLine?: number, manual?: boolean) => {
    setSelection(matches.length === 1 ? matches[0]!.path : "");
    setSelectedAnchor(anchor);
    setDialog({ kind: "link", from, link, matches, sourceLine, manual });
  };
  const repairReference = async (proposal: Extract<KnowledgeDialog, { kind: "link" }>) => {
    const current = getNote(proposal.from);
    if (!current) return;
    const anchor = selectedAnchor.trim() || undefined;
    try {
      const markdown = await invoke<string>("repair_note_wikilink", {
          markdown: current.content, reference: proposal.link.raw, sourceLine: proposal.sourceLine ?? null,
          destination: selection, anchor: anchor ?? null,
          ...(proposal.link.syntax === "markdown" ? { markdownSourcePath: proposal.from } : {}),
          ...(proposal.link.sourceOffset !== undefined ? { sourceOffset: proposal.link.sourceOffset } : {}),
        });
      if (getNote(proposal.from)?.content !== current.content) {
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
    try {
      if (!noteIndex.index.data) await noteIndex.index.refetch();
      const result = await resolveRepositoryLink(checkout.checkoutId, link, from);
      if (result?.status === "resolved" && action !== "repair") {
        const { path, line, anchor } = result.location;
        open(path, line ?? undefined, anchor ?? undefined, action === "side" ? "side" : undefined);
      } else if (result?.status === "attachment") {
        setDialog({ kind: "repositoryAttachment", paths: result.paths, label: link.alias ?? link.target });
      } else {
        const targets = result?.status === "ambiguous" ? result.candidates.map(asNoteLocation)
          : result?.status === "missing_anchor" || result?.status === "resolved" ? [asNoteLocation(result.location)] : [];
        if (targets.length === 1) void loadNote(targets[0]!.path);
        offerLinkRepair(from, link, targets, targets.length === 1 ? link.anchor : undefined, link.sourceLine,
          link.syntax === "markdown" && (!link.editableInline));
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
    setPropertyFilter(undefined);
    setQuery("#" + tag); setSidebar(true); setDisplay("search"); requestSearchFocus((before) => before + 1);
  };
  const searchForProperty = ({ name, value }: NotePropertyFilter) => {
    setPropertyFilter({ name, value });
    setQuery(""); setSidebar(true); setDisplay("search"); requestSearchFocus((before) => before + 1);
  };
  const propertySource = (path: string) => {
    const current = getNote(path);
    return current && !current.sourceProblem && !current.incoming && current.external === undefined ? current.content : undefined;
  };
  const propertyActions = (path: string, { name, value, line, filterable }: NoteYamlProperty): MenuGroups[number] => [
    { label: "Go to property in source", run: () => open(path, line, undefined, "source") },
    { label: "Copy property name", run: () => menu.copy(name, "Copied property name") },
    { label: "Copy property value", run: () => menu.copy(value, "Copied property value") },
    ...(filterable ? [{ label: "Find notes with this property value", run: () => searchForProperty({ name, value }) }] : []),
  ];
  const tagActions = (tag: string, ownerPanel?: FilePanelApi): MenuGroups[number] => [
    { label: "Copy tag", run: () => menu.copy("#" + tag, "Copied tag") },
    { label: "Search notes for tag", run: () => searchForTag(tag) },
    {
      label: "Rename tag in checkout…", run: () => setDialog({ kind: "tagRename", tag, ownerPanel }),
    },
  ];
  const noteContextActions = ({ path, line, anchor, anchorKind, anchorLine: hitAnchorLine, link, attachment, tag, excerpt, selection: noteSelection, property, mermaid, diagramImage }: NoteContextHit, reading: boolean, ownerPanel?: FilePanelApi): MenuGroups => {
    const groups: MenuGroups[number][] = [];
    const propertySnapshot = notes[path]?.content;
    const properties = propertySnapshot === undefined ? [] : propertiesOf(propertySnapshot).rows.filter((row) => row.line === line
      && (!property || row.name === property.name && row.value === property.value));
    const selectedProperty = properties.length === 1 ? properties[0] : undefined;
    if (attachment) groups.push(...attachmentActions(attachment.paths, attachment.label));
    if (link) {
      const from = link.sourcePath ?? path;
      const resolved = lookupLink(link, from);
      const target = resolved?.status === "resolved" ? noteWikilink(resolved.location.path, resolved.location.anchor ?? undefined) : undefined;
      const manual = link.syntax === "markdown" && !link.editableInline;
      const editSource = () => open(from, link.sourceLine, undefined, "source");
      const isAttachment = resolved?.status === "attachment";
      if (isAttachment) groups.push(...attachmentActions(resolved.paths, link.alias ?? link.target));
      else groups.push([
        { label: "Open link", run: () => { void navigateLink(from, link); } },
        ...(!isAttachment ? [{ label: "Open link to the side", run: () => { void navigateLink(from, link, "side"); } }] : []),
        ...(!isAttachment ? [{ label: manual && link.sourceLine ? "Edit Markdown link in source" : resolved?.status === "resolved" ? "Change link target…" : "Resolve note link…",
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
          { label: `Add ${kind} to conversation…`, run: () => conversationActions.startExcerpt({ path, ...block }) },
        ] : []),
      ]);
    }
    if (reading && line && !selectedProperty) groups.push([{ label: anchorKind === "heading" ? "Edit heading in source" : "Go to source",
      run: () => open(path, hitAnchorLine ?? line, anchor, "source") }]);
    if (selectedProperty) groups.push([
      ...(selectedProperty.editable && propertySnapshot !== undefined && propertySource(path) === propertySnapshot ? [{
        label: "Edit property…", run: () => setDialog({ kind: "property", path, property: selectedProperty, source: propertySnapshot, ownerPanel }),
      }] : []),
      ...propertyActions(path, selectedProperty),
    ]);
    else if (property) groups.push([
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
      ...(excerpt.text.trim() ? [{ label: "Add selection to conversation…", run: () => conversationActions.startExcerpt(excerpt) }] : []),
      ...(!reading && selectionSource?.fileIdentity
        && !selectionSource.incoming && !selectionSource.sourceProblem && noteSelection?.text.trim() ? [{
        label: "Extract selection to a new note…", run: () => setDialog({ kind: "extract", path, selection: noteSelection, ownerPanel }),
      }] : []),
    ]);
    const startLine = excerpt?.path === path ? excerpt.startLine : anchor ? line : undefined;
    const endLine = excerpt?.path === path ? excerpt.endLine : startLine;
    if (startLine && endLine && notes[path]) {
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
    { label: "Show file history", run: () => openWorkspace("git", fileHistoryTarget(path)) },
  ]];

  return (
    <section ref={workspace} className="file-workspace knowledge-workspace" aria-label="Knowledge workspace" onKeyDown={(event) => {
      if (!event.ctrlKey && !event.metaKey) return;
      if (event.key.toLowerCase() === "s" && active && note) { event.preventDefault(); save(active); }
      if (event.key.toLowerCase() === "p") {
        event.preventDefault(); setSidebar(true); setDisplay("search"); requestSearchFocus((before) => before + 1);
      }
    }}>
      <FileSidebar name="Knowledge navigation" open={sidebar && !(sidebarDrawer && detailsDrawer && inspector && !graph)} drawer={sidebarDrawer} onClose={closeNavigation}>
        <div className="file-sidebar-heading">
          <BookOpen size={16} /><h2>Notes</h2>
          <div className="file-toolbar">
            <button type="button" className="fw-icon" aria-label="Show notes" title="Show notes" aria-pressed={display === "tree"} onClick={() => setDisplay("tree")}><ListTree size={15} /></button>
            <ControlTooltip label="Search notes" shortcut="Ctrl+P"><button type="button" className="fw-icon" aria-label="Search notes" aria-pressed={display === "search"} onClick={() => setDisplay("search")}><Search size={16} aria-hidden="true" /></button></ControlTooltip>
          </div>
        </div>
        <div className="file-sidebar-scroll">
          {display === "search" ? <KnowledgeSearch query={query} onQueryChange={setQuery} results={matches}
            pending={propertyFilter ? propertySearch.pending : noteIndex.searching}
            error={(propertyFilter ? noteIndex.index.error ?? propertySearch.error : noteIndex.error) ?
              String(propertyFilter ? noteIndex.index.error ?? propertySearch.error : noteIndex.error) : undefined}
            filter={propertyFilter && <div className="file-field">
              <span className="min-w-0 flex-1 break-words">{propertyFilter.name}: {propertyFilter.value || "(empty)"}</span>
              <button type="button" className="fw-btn" aria-label="Clear property filter" onClick={() => setPropertyFilter(undefined)}>Clear</button>
            </div>}
            focusRequest={focusSearch} onOpen={(location) => chooseNote(location.path, location.line ?? undefined, location.anchor ?? undefined)} />
            : <FileTree paths={catalog.map(({ path }) => path)} folders={treeActions.folders} active={active} onOpen={chooseNote} onOpenNoteFavorite={openNoteFavorite} checkoutId={checkout.checkoutId} workspace="knowledge" reveal={reveal} actions={(item, selection) => selection ? [
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
              [{ label: "Open terminal here", run: () => openWorkspace("ide", terminalFolderTarget(item.kind === "file" ? containingFolder(item.path) : item.path)) }],
              ...menu.location(item),
              ...(item.kind === "file" ? [comparison.actions(item.path)] : []),
              ...conversationActions.actions(item),
              ...fileOperations.actions(item),
            ]} />}
        </div>
      </FileSidebar>
      <div className="file-workspace-main" ref={detailSpace}>
        <header className="file-workspace-header">
          <SidebarToggle open={sidebar} setOpen={setSidebar} label="Toggle Knowledge navigation" buttonRef={sidebarToggle} />
          <BookOpenText size={17} /><h1>Knowledge</h1>
          <div className="file-toolbar">
            <button type="button" className="fw-icon" title="Rebuild note index" aria-label="Rebuild note index"
              disabled={noteIndex.index.isFetching} onClick={() => { void noteIndex.index.refetch(); }}><RotateCw size={15} /></button>
            <button type="button" className="fw-btn note-graph-toggle" aria-pressed={Boolean(graph)} onClick={() => setGraph(graph ? false : "global")}><Network size={15} />Graph</button>
            <ControlTooltip label="Note details"><button ref={inspectorToggle} type="button" className="fw-icon" aria-label="Note details"
              aria-expanded={Boolean(inspector && !graph)} onClick={() => { setGraph(false); setInspector(inspector && !graph ? null : "outline"); }}><PanelRight size={16} aria-hidden="true" /></button></ControlTooltip>
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
            <ControlTooltip label="Save note" shortcut="Ctrl+S"><button type="button" className="fw-icon" aria-label="Save note" disabled={!isDirty(note)}
              onClick={() => { if (active) save(active); }}><Save size={16} aria-hidden="true" /></button></ControlTooltip>
            <button type="button" className="fw-icon" title="Split Reading view" aria-label="Split Reading view" disabled={!note}
              onClick={() => {
                if (!active) return;
                if (mode === "reading") setEditAlongsideReading((before) => new Set(before).add(active));
                dock.split(active, "reading");
              }}><SplitSquareHorizontal size={15} /></button>
            <button type="button" className="fw-icon" title="Rename note" aria-label="Rename note"
              disabled={!note || isDirty(note) || Boolean(active && repository.notes[active]?.sourceProblem)}
              onClick={() => { if (active) void requestRename(active); }}><FilePenLine size={15} /></button>
          </div>
        </div>}
        <div className="note-body">
          <div className="file-dock-body" hidden={Boolean(graph)}>
            <FileDock onReady={dock.onReady} context={{
              renderFile: (path, view, panelId) => {
                const entry = notes[path];
                if (!entry) {
                  const failure = repository.failures[path];
                  return failure
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
                        { label: "Open terminal here", run: () => openWorkspace("ide", terminalFolderTarget(item.kind === "file" ? containingFolder(item.path) : item.path)) }],
                      ...menu.location(item),
                    ]} />
                    {sourceProblem && <p role="alert" className="file-help">{sourceProblem}</p>}
                    {path !== active && entry.external !== undefined && <button type="button" className="fw-btn fw-outlined"
                      onClick={() => reviewExternalChange(path, entry.external!)}>Review external change</button>}
                    <Suspense fallback={<p role="status" className="file-help">Loading note view…</p>}>
                      {view === "editor" && <div className="note-editable-view" hidden={selectedMode === "reading"}>
                        <Editor mode={selectedMode === "source" ? "source" : "live"} path={path} source={entry.content} files={catalog} jump={jumps[path]} analyze
                          suggestLinks={(prefix) => suggestRepositoryLinks(checkout.checkoutId, path, prefix)}
                          onChange={(content) => update(path, content)} onLink={onLink}
                          contextActions={(hit) => noteContextActions(hit, false, ownerPanel)} />
                      </div>}
                      {selectedMode === "reading" && <Reading path={path} content={entry.content} files={catalog} jump={jumps[path]} onLink={onLink}
                        properties={propertiesOf(entry.content).rows}
                        checkoutId={checkout.checkoutId}
                        resolveLinks={lookupLink}
                        previewFor={previewNote} contextActions={(hit) => noteContextActions(hit, true, ownerPanel)}
                        onRepositoryAttachment={(paths, label) => setDialog({ kind: "repositoryAttachment", paths, label })} />}
                    </Suspense>
                  </div>
                );
              },
              empty: <FileUnavailable title="Open a note"
                detail="Choose a note from the tree or search by title and content." />,
              dirty: dirtyNote,
              requestClose: close,
              layoutActions: (api) => dock.layoutActions(api, dockCommands),
              groupActions: (id) => dock.groupActions(id, dockCommands),
              tabActions: (path, view, api) => [[
                { label: "Save", disabled: !isDirty(notes[path]), run: () => { void save(path); } },
                { label: "Split right", run: () => dock.split(path, view, "right", api.id) },
                { label: "Split down", run: () => dock.split(path, view, "below", api.id) },
                ...(view === "editor" ? [{ label: "Open Reading to the side", run: () => open(path, undefined, undefined, { readingBeside: api.id }) }] : []),
                { label: "Reveal in note tree", run: () => revealNote(path) },
              ], ...noteMenu(path, api).slice(1), ...menu.location({ kind: "file", path }), comparison.actions(path, api), ...conversationActions.actions({ kind: "file", path }), ...fileOperations.actions({ kind: "file", path }), [
                { label: "Close tab", run: () => close(path, api) },
              ]],
            }} />
          </div>
          {graph && <div className="note-graph-host">
            {(graphEdges.error || noteIndex.index.error) ? <p role="alert" className="file-help">{String(graphEdges.error ?? noteIndex.index.error)}</p>
              : <Suspense fallback={<p role="status" className="file-help">Loading note graph…</p>}>
                {graphProjection ? <NoteGraph key={localGraph?.request ?? "global"} initialCenter={localGraph?.path}
                  projection={graphProjection} selected={active} onOpen={open} onNodeActions={graphNodeActions} />
                  : <p role="status" className="file-help">Loading note graph…</p>}
              </Suspense>}
          </div>}
          {inspector && !graph && <WorkbenchPanel as="aside" className="note-inspector" aria-label="Note details"
            drawer={detailsDrawer} open drawerClass="note-inspector-drawer" label="Note details" onClose={closeInspector}
            edge="left" resizeKey="note-details" resizeLabel="note details">
            <div className="note-inspector-views">
              {([[ListTree, "outline", "Outline"], [Link2, "links", "Links"], [FileText, "properties", "Properties"], [Paperclip, "attachments", "Attachments"]] as const)
                .map(([Icon, view, label]) => <ControlTooltip label={label} key={view}><button type="button" className="fw-icon" aria-label={label} aria-pressed={view === inspector}
                  onClick={() => setInspector(view)}><Icon size={16} aria-hidden="true" /></button></ControlTooltip>)}
              <ControlTooltip label="Close note details"><button type="button" className="fw-icon ml-auto" aria-label="Close note details"
                onClick={closeInspector}><X size={16} aria-hidden="true" /></button></ControlTooltip>
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
                    if (targets.length === 1) void loadNote(targets[0]!.path);
                    offerLinkRepair(active, { ...link, raw: entry.rawTarget }, targets.map((target) => ({ path: target.path, anchor: target.anchor ?? undefined })),
                      resolution.kind === "missing_anchor" ? link.anchor : undefined, entry.source.line ?? undefined, !link.wiki);
                  }} />}
                {inspector === "properties" && <section aria-label="Properties"><h3>Properties</h3>
                  {activeProperties.pending && <p role="status" className="file-help">Reading YAML properties…</p>}
                  {activeProperties.error && <p role="alert" className="file-help">{String(activeProperties.error)}</p>}
                  {activeProperties.rows.map((property, index) =>
                    <NotePropertyEditor key={`${active}:${property.name}:${index}`} name={property.name} value={property.value}
                      editable={property.editable && propertySource(active) !== undefined} read={() => propertySource(active)}
                      update={(content) => update(active, content)}
                      actions={propertyActions(active, property)} />)}
                  {activeProperties.rows.length === 0 && !activeProperties.pending && !activeProperties.error &&
                    <p className="file-help">This note has no frontmatter properties.</p>}
                  {!isDirty(note) && previewNote(active)?.tags?.length ? <ul aria-label="Tags in this note" className="flex flex-wrap gap-1 pt-2">
                    {previewNote(active)?.tags.map((tag) => <li key={tag}>
                      <ContextActions label={"#" + tag} groups={[tagActions(tag)]}>
                        <button type="button" className="fw-btn" onClick={() => searchForTag(tag)}>#{tag}</button>
                      </ContextActions>
                    </li>)}
                  </ul> : null}
                </section>}
                {inspector === "attachments" && <section aria-label="Attachments"><h3>Attachments</h3>
                  {outgoing.flatMap((link, index) => {
                    const result = lookupLink(link, active);
                    const label = link.alias ?? link.target;
                    return result?.status === "attachment" ? [<ContextActions key={index} label={label} groups={attachmentActions(result.paths, label)}>
                      <button type="button" className="note-attachment-row" onClick={() => navigateLink(active, link)}>{label}</button>
                    </ContextActions>] : [];
                  })}
                </section>}
              </>}
            </div>
          </WorkbenchPanel>}
        </div>

        {active && note && !graph && <footer className="ide-statusbar note-statusbar" aria-label="Note status and actions">
          <span>{note.external !== undefined ? "External changes" : isDirty(note) ? "Unsaved changes" : savedNote?.sourceProblem ? "Note unavailable" : "Saved"}</span>
          <div className="note-status-actions">
            {note.external !== undefined && <button type="button" className="fw-btn"
              onClick={() => reviewExternalChange(active, note.external!)}>Review external change</button>}
            {savedNote && <button type="button" className="fw-btn" disabled={!canLaunchFromNote}
              title={canLaunchFromNote ? "Choose a published workflow and review this revision" : "A saved note without conflicts is required"}
              onClick={() => setDialog({ kind: "launch", source: savedNote.base })}><Wand2 size={14} />Run workflow from note</button>}
          </div>
        </footer>}
      </div>

      {treeActions.dialog}
      {filePreview.dialog}
      {conversationActions.dialog}
      {dialog?.kind === "property" && <FileDialog title="Edit YAML property" description={dialog.path} ownerPanel={dialog.ownerPanel} onClose={closeDialog}>
        <NotePropertyEditor name={dialog.property.name} value={dialog.property.value} expectedSource={dialog.source} focus
          read={() => propertySource(dialog.path)}
          update={(content) => { update(dialog.path, content); closeDialog(); setStatus("Updated property in the note draft."); }} />
      </FileDialog>}
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
            const { path } = dialog;
            void repository.resolve(path, false).then(closeDialog, (failure: unknown) => setProblem(String(failure)));
          }}>Load external version</button>
          <button type="button" className="fw-btn fw-primary" onClick={() => {
            const { path } = dialog;
            void repository.resolve(path, true).then(closeDialog, (failure: unknown) => setProblem(String(failure)));
          }}>Keep local edits</button>
        </div>
        {problem && <p role="alert">{problem}</p>}
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
      {dialog?.kind === "link" && <FileDialog title={dialog.matches.length > 1 ? "Choose a note" : "Repair note link"}
        description={dialog.link.syntax === "markdown" ? `Resolve ${dialog.link.raw} from ${dialog.from}.` : `Resolve [[${dialog.link.raw}]] from ${dialog.from}.`} onClose={closeDialog}>
        <label>Target note<select value={selection} onChange={(event) => {
          const path = event.target.value;
          const originalAnchor = dialog.link.anchor;
          setSelection(path);
          setSelectedAnchor(originalAnchor ?? "");
          if (path) void loadNote(path);
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
      {dialog?.kind === "repositoryAttachment" && <FileDialog title={dialog.label} description="Repository attachment" onClose={closeDialog}>
        <RepositoryNoteMedia checkoutId={checkout.checkoutId} paths={dialog.paths} title={dialog.label} allowSaveCopy />
      </FileDialog>}
    </section>
  );
}
