import {
  AlertCircle, CircleAlert, Code2, FileSearch2, Files, FolderTree, GitCompareArrows,
  PanelBottom, Save, SplitSquareHorizontal, TerminalSquare, Trash2,
} from "lucide-react";
import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import {
  FileCaption, FileDialog, FileDock, FileSidebar, FileTree, FileUnavailable,
  keepOnlyCleanClose, SidebarToggle, useFileDock, useFileSidebar, type FilePanelApi,
} from "@/ide-file-workspace";
import { ExampleFileSearch, matchReference, noteSearchTarget, ProjectFileSearch, readFolderSearchTarget, type FileSearchScope } from "@/ide-file-search";
import { noteWindowTarget } from "@/file-editor-popout";
import { ContextActions, type MenuGroups } from "@/components/ui/context-actions";
import { containingFolder, fileContextActions, fileHistoryTarget, noteRenameTarget, openNotesTarget, readSourceLineTarget, readTerminalFolderTarget, useCheckoutTextReader, workingDiffTarget } from "@/file-context-actions";
import { useCheckoutTreeActions } from "@/checkout-tree-actions";
import { useCheckoutConversationActions } from "@/checkout-conversation-actions";
import { changedItemPath, itemIncludesPath, useCheckoutItemOperations } from "@/checkout-item-operations";
import { useCheckoutFileComparison } from "@/checkout-file-comparison";
import { exampleNotes, exampleSources, fileName, findFileText, normalizeRepositoryPath, type FileSearchResult } from "@/ide-knowledge-data";
import type { CodeModel, CodeProblem, EditorLocation, EditorSettings } from "@/ide-code-editor";
import { EditorStatus } from "@/ide-editor-status";
import { SourceFormatDialog } from "@/source-format-dialog";
import type { SourceFormatChange } from "@/source-file-format";
import { IdeProblems, problemReference, type FileProblem } from "@/ide-problems";
import { useTerminalSessions } from "@/ide-terminal-sessions";
import { isTerminalReviewTarget, takeTerminalReview } from "@/terminal-reviewed-input";
import { ResizablePanel } from "@/panel-resize";
import { ControlTooltip } from "@/components/ui/control-tooltip";
import { useWorkspaceSize } from "@/workbench-panel";
import { useRepositorySourceBuffers, type NoteBuffer } from "@/repository-note-buffers";
import { SourceFormatDialog } from "@/source-format-dialog";
import type { SourceFormatChange } from "@/source-file-format";
import type { WorkspaceProps } from "@/workbench-contract";

const Editor = lazy(() => import("@/ide-code-editor").then(({ CodeEditor }) => ({ default: CodeEditor })));
const Terminal = lazy(() => import("@/ide-terminal").then(({ ExampleTerminal }) => ({ default: ExampleTerminal })));
const WorktreeTerminal = lazy(() => import("@/ide-terminal").then(({ ManualTerminal }) => ({ default: ManualTerminal })));

type IdeDialog =
  | { kind: "close"; path: string; api: FilePanelApi }
  | { kind: "format"; path: string; change: SourceFormatChange }
  | { kind: "conflict"; path: string };

const markdownPath = (path: string) => /\.(?:md|markdown)$/i.test(path);
const editorText = (content: string) => content.replace(/\r\n?/g, "\n");

function newBuffers(simulated: boolean, recovery: boolean): Record<string, NoteBuffer> {
  return Object.fromEntries((simulated ? exampleSources : []).map(({ path, content }) => [
    path,
    { content: recovery && path === "src/health.ts" ? content + "\n// Recovered readiness checks\n" : content, saved: content, revision: 1 },
  ]));
}

export default function IdeWorkspace(props: WorkspaceProps) {
  const scope = `${props.checkout.checkoutId}:${props.simulation?.epoch ?? "real"}`;
  return <IdeSurface key={scope} {...props} scope={scope} />;
}

function IdeSurface({ checkout, target, simulation, startSimulation, openWorkspace, openChanges, scope }: WorkspaceProps & { scope: string }) {
  const simulated = Boolean(simulation);
  const requestedFolder = readFolderSearchTarget(target);
  const requestedTerminal = readTerminalFolderTarget(target);
  const firstPath = simulated ? target && requestedFolder === undefined && requestedTerminal === undefined && !markdownPath(target) ? normalizeRepositoryPath(target) : "src/health.ts" : undefined;
  const dock = useFileDock({ checkoutId: checkout.checkoutId, workspace: "ide", simulated, initialPath: firstPath });
  const source = useRepositorySourceBuffers(checkout.checkoutId, !simulated);
  const { load: loadSource, watch: watchSource, update: updateSource } = source;
  const workspace = useRef<HTMLElement>(null);
  const sidebarToggle = useRef<HTMLButtonElement>(null);
  const [sidebar, setSidebar, sidebarDrawer] = useFileSidebar(useWorkspaceSize(workspace).width);
  const [navigator, setNavigator] = useState<"files" | "search">(requestedFolder === undefined ? "files" : "search");
  const [searchScope, setSearchScope] = useState<FileSearchScope | undefined>(() => requestedFolder ? { kind: "folder", path: requestedFolder } : undefined);
  const [reveal, setReveal] = useState<{ path: string }>();
  const [query, setQuery] = useState("");
  const [examples, setExamples] = useState(() => newBuffers(simulated, simulation?.scenario === "recovery"));
  const files: Record<string, NoteBuffer> = simulated ? examples : source.files;
  const [jumps, setJumps] = useState<Record<string, { line: number; column?: number; token: number }>>({});
  const [markers, setMarkers] = useState<Record<string, CodeProblem[]>>({});
  const [editorSettings, setEditorSettings] = useState<Record<string, EditorSettings>>({});
  const [quickFix, setQuickFix] = useState<(EditorLocation & { panelId: string })>();
  const [bottom, setBottom] = useState<"terminal" | "problems" | null>("terminal");
  const terminals = useTerminalSessions();
  const { openDirectory, openReview } = terminals;
  const [terminalRename, setTerminalRename] = useState<{ id: string; title: string }>();
  const [exampleClearRevision, setExampleClearRevision] = useState(0);
  const openTerminal = useCallback((folder: string) => {
    setBottom("terminal");
    openDirectory(folder);
  }, [openDirectory]);
  const [status, setStatus] = useState(simulation?.scenario === "recovery" ? "Recovered unsaved changes in src/health.ts" : "");
  const [problem, setProblem] = useState("");
  const [dialog, setDialog] = useState<IdeDialog>();
  const [confirmingClose, setConfirmingClose] = useState(false);
  const models = useRef(new Map<string, CodeModel>()).current;
  const lastTarget = useRef(firstPath);
  const openRequest = useRef(0);
  const active = dock.path;
  const buffer = active ? files[active] : undefined;
  const dirty = (path: string) => {
    const entry = simulated ? files[path] : source.getFile(path);
    return Boolean(entry && (entry.external !== undefined || entry.content !== entry.saved));
  };
  const filesForSearch = [
    ...Object.entries(examples).map(([path, value]) => ({ path, content: value.content })),
    ...(simulated ? exampleNotes : []),
  ];
  const results = findFileText(filesForSearch.filter(({ path }) => !searchScope || (searchScope.kind === "file"
    ? path === searchScope.path : path.startsWith(`${searchScope.path}/`))), query, true);
  const paths = simulated ? filesForSearch.map(({ path }) => path)
    : [...new Set([...source.catalog, ...source.draftPaths, ...Object.keys(source.files)])];
  const markerEntries = Object.entries(markers).flatMap(([path, items]) => items.map((item) => ({ path, ...item })));

  useEffect(() => () => {
    openRequest.current += 1;
    for (const { onChange, model } of models.values()) {
      onChange.dispose();
      model.dispose();
    }
    models.clear();
  }, [models]);

  useEffect(() => {
    for (const [path, record] of models) {
      const entry = files[path];
      if (!entry || editorText(record.model.getValue()) === entry.content) continue;
      record.synchronizing = true;
      try { record.model.setValue(entry.content); } finally { record.synchronizing = false; }
    }
  }, [files, models]);

  const openFile = useCallback((path: string, line?: number, split = false, column?: number, newWindow = false) => {
    const request = ++openRequest.current;
    if (markdownPath(path)) {
      const location = line !== undefined || split ? noteSearchTarget(path, line ?? 1, request, split) : path;
      openWorkspace("knowledge", newWindow ? noteWindowTarget(path, request) : location);
      return;
    }
    const focus = (file: string) => {
      if (line !== undefined) setJumps((before) => ({ ...before, [file]: { line, column, token: (before[file]?.token ?? 0) + 1 } }));
      if (newWindow) dock.openInWindow(file);
      else if (split) dock.split(file);
      else dock.open(file);
    };
    if (simulated) {
      focus(path);
      return;
    }
    void loadSource(path).then((canonical) => {
      if (!canonical || request !== openRequest.current) return;
      watchSource(canonical);
      focus(canonical);
    });
  }, [dock.open, dock.openInWindow, dock.split, loadSource, openWorkspace, simulated, watchSource]);

  function chooseFile(path: string, line?: number) {
    openFile(path, line);
    if (sidebarDrawer) {
      setSidebar(false);
      if (!markdownPath(path)) requestAnimationFrame(() => sidebarToggle.current?.focus({ preventScroll: true }));
    }
  }

  useEffect(() => {
    if (simulated || !dock.ready || !dock.api) return;
    const api = dock.api;
    let live = true;
    const paths = new Set(api.panels.flatMap((panel) => {
      const path = (panel.params as { path?: string } | undefined)?.path;
      return path ? [path] : [];
    }));
    for (const path of paths) {
      void loadSource(path).then((canonical) => {
        if (!live || !canonical || !api.panels.some((panel) => panel.params?.path === path)) return;
        watchSource(canonical);
        if (canonical !== path) {
          dock.open(canonical);
          for (const panel of api.panels) if (panel.params?.path === path) panel.api.close();
        }
      });
    }
    return () => { live = false; };
  }, [dock.api, dock.open, dock.ready, loadSource, simulated, watchSource]);

  useEffect(() => {
    if (!target || !dock.ready) return;
    if (isTerminalReviewTarget(target)) {
      const request = !simulated && takeTerminalReview(target, checkout.checkoutId);
      if (request) {
        setBottom("terminal");
        openReview(request.target, request.text);
      }
      return;
    }
    if (requestedTerminal !== undefined) {
      openTerminal(requestedTerminal);
      return;
    }
    if (requestedFolder !== undefined) {
      setSidebar(true);
      setSearchScope(requestedFolder ? { kind: "folder", path: requestedFolder } : undefined);
      setNavigator("search");
      return;
    }
    const location = readSourceLineTarget(target);
    const next = normalizeRepositoryPath(location?.path ?? target);
    const request = location ? target : next;
    if (next && request !== lastTarget.current) {
      lastTarget.current = request;
      openFile(next, location?.line, false, location?.column);
    }
  }, [checkout.checkoutId, dock.ready, openFile, openReview, openTerminal, requestedFolder, requestedTerminal, setSidebar, simulated, target]);

  const update = useCallback((path: string, content: string) => {
    content = editorText(content);
    if (!simulated) {
      updateSource(path, content);
      return;
    }
    setExamples((before) => {
      const entry = before[path];
      return entry && entry.content !== content ? { ...before, [path]: { ...entry, content } } : before;
    });
  }, [simulated, updateSource]);
  const report = useCallback((path: string, issues: CodeProblem[]) => setMarkers((before) => ({ ...before, [path]: issues })), []);
  const reportEditorSettings = useCallback((path: string, settings: EditorSettings) => setEditorSettings((before) => ({ ...before, [path]: settings })), []);
  const finishQuickFix = useCallback(() => setQuickFix(undefined), []);
  const showQuickFix = ({ path, line, column }: FileProblem) => {
    if (dock.api?.activePanel?.params?.path !== path) dock.open(path);
    const panel = dock.api?.activePanel;
    if (panel?.params?.path === path) setQuickFix({ panelId: panel.id, line, column });
  };

  const save = async (path: string) => {
    const entry = simulated ? files[path] : source.getFile(path);
    if (!entry) return false;
    if (entry.external !== undefined) {
      dock.cancelCloseBatch();
      setDialog({ kind: "conflict", path });
      return false;
    }
    setProblem("");
    if (!simulated) {
      const saving = `Saving ${path}…`;
      setStatus(saving);
      source.clearError();
      if (!await source.save(path)) {
        setStatus((current) => current === saving ? "" : current);
        if (source.getFile(path)?.external !== undefined) {
          dock.cancelCloseBatch();
          setDialog({ kind: "conflict", path });
        }
        return false;
      }
    } else {
      setExamples((before) => {
        const current = before[path];
        if (!current) return before;
        return { ...before, [path]: { ...current, saved: current.content, revision: Number(current.revision) + 1 } };
      });
    }
    setStatus(`Saved ${path}`);
    return true;
  };

  const resolve = async (path: string, keepLocal: boolean) => {
    setProblem("");
    if (!simulated) {
      try {
        source.clearError();
        await source.resolve(path, keepLocal);
        return true;
      } catch (failure) {
        setProblem(String(failure));
        return false;
      }
    }
    const entry = files[path];
    if (!entry) return false;
    const saved = entry.external ?? entry.saved;
    setExamples((before) => ({
      ...before, [path]: { content: keepLocal ? entry.content : saved, saved, revision: Number(entry.revision) + Number(entry.external !== undefined) },
    }));
    return true;
  };
  const forgetFile = (path: string) => {
    if (!simulated) source.forget(path);
    const record = models.get(path);
    record?.onChange.dispose();
    record?.model.dispose();
    models.delete(path);
    setMarkers((before) => {
      const next = { ...before };
      delete next[path];
      return next;
    });
    setEditorSettings((before) => {
      const next = { ...before };
      delete next[path];
      return next;
    });
  };
  const closeView = (path: string, api: FilePanelApi) => {
    const remains = dock.api && keepOnlyCleanClose(dock.api, path, api);
    if (!dock.closeTab(api)) return false;
    if (!remains) forgetFile(path);
    return true;
  };
  const requestClose = (path: string, api: FilePanelApi) => {
    if (!dirty(path) || !dock.api || keepOnlyCleanClose(dock.api, path, api)) {
      closeView(path, api);
      return true;
    }
    setDialog({ kind: "close", path, api });
    return false;
  };
  const dockCommands = { dirty, requestClose, loadClosedFile: async (path: string) => {
    if (simulated) return files[path] ? path : undefined;
    const canonical = await loadSource(path);
    if (canonical) watchSource(canonical);
    return canonical;
  } };
  const confirmClose = async ({ path, api }: Extract<IdeDialog, { kind: "close" }>, saveChanges: boolean) => {
    if (confirmingClose) return;
    setConfirmingClose(true);
    try {
      if (await (saveChanges ? save(path) : resolve(path, false)) && closeView(path, api)) {
        setDialog(undefined);
        dock.resumeCloseBatch(dockCommands);
      }
    } catch (failure) { setProblem(String(failure)); }
    finally { setConfirmingClose(false); }
  };
  const cancelClose = () => {
    if (confirmingClose) return;
    dock.cancelCloseBatch();
    setDialog(undefined);
  };
  const readOpenFile = useCallback((path: string) => dock.api?.panels.some((panel) => panel.params?.path === path)
    ? source.getFile(path)?.content : undefined, [dock.api, source.getFile]);
  useCheckoutTextReader(checkout.checkoutId, !simulated, readOpenFile);
  const menu = fileContextActions({ checkout, simulated, status: setStatus, report: setProblem,
    currentText: (path) => simulated ? examples[path]?.content ?? exampleNotes.find((note) => note.path === path)?.content : readOpenFile(path),
  });
  const comparison = useCheckoutFileComparison({ checkoutId: checkout.checkoutId, simulated, bufferAt: (path) => {
    const note = simulated && exampleNotes.find((entry) => entry.path === path);
    return files[path] ?? (note ? { content: note.content, saved: note.content } : undefined);
  }, copy: menu.copy, report: setProblem, openHistory: (path) => openWorkspace("git", fileHistoryTarget(path)),
  canRestore: (path) => !markdownPath(path), restoreLocalText: async (path, content) => {
    const canonical = await loadSource(path);
    if (canonical !== path) throw new Error("This file now resolves to a different path.");
    await source.loadLocalRevision(path, content);
    watchSource(path);
    dock.open(path);
    setStatus(`Loaded a local revision of ${path} into the editor`);
  } });
  async function openSelected(paths: readonly string[]) {
    const request = ++openRequest.current;
    const code = paths.filter((path) => !markdownPath(path));
    const notes = paths.filter(markdownPath);
    const loaded = simulated ? code : await Promise.all(code.map(loadSource));
    if (request !== openRequest.current) return;
    for (const path of loaded) {
      if (path) { if (!simulated) watchSource(path); dock.open(path); }
    }
    if (notes.length) openWorkspace("knowledge", openNotesTarget(notes));
  }
  const revealFile = (path: string) => { setSidebar(true); setNavigator("files"); setReveal({ path }); };
  const treeActions = useCheckoutTreeActions({ checkoutId: checkout.checkoutId, simulated, refresh: source.refreshCatalog, report: setProblem,
    onCreated: (path, kind) => {
      setStatus(`Created ${path}`);
      if (kind === "folder") setReveal({ path });
      else openFile(path);
    },
  });
  const conversationActions = useCheckoutConversationActions(checkout.checkoutId, simulated, (id) => openWorkspace("conversations", id));
  const renameNote = (path: string) => openWorkspace("knowledge", noteRenameTarget(path));
  const fileOperations = useCheckoutItemOperations({ checkoutId: checkout.checkoutId, simulated,
    folders: treeActions.folders, refreshTree: treeActions.refreshTree, report: setProblem, status: setStatus,
    isDirty: (items) => Object.keys(files).some((path) => dirty(path) && items.some((item) => itemIncludesPath(item, path))),
    onRenameNote: renameNote, onReveal: revealFile,
    onChanged: (edits) => {
      const next = dock.reconcilePaths((path) => {
        if (dirty(path)) return undefined;
        const destination = changedItemPath(path, edits);
        return destination && markdownPath(destination) ? null : destination;
      });
      for (const path of Object.keys(files)) if (!dirty(path) && changedItemPath(path, edits) !== undefined) forgetFile(path);
      for (const path of next) void loadSource(path).then((canonical) => { if (canonical) watchSource(canonical); });
    },
  });
  const openSearchMatch = (path: string, line: number, column?: number) => openFile(path, line, false, column);
  const searchInFolder = (path: string) => {
    setSidebar(true);
    setSearchScope(path ? { kind: "folder", path } : undefined);
    setNavigator("search");
  };
  const searchScopeActions = (path: string) => {
    const folder = containingFolder(path);
    return [
      ...(searchScope?.kind === "file" && searchScope.path === path ? [] : [{ label: "Search in this file", run: () => setSearchScope({ kind: "file", path }) }]),
      ...(folder ? searchScope?.kind === "folder" && searchScope.path === folder ? [] : [{ label: "Search in containing folder", run: () => searchInFolder(folder) }]
        : searchScope ? [{ label: "Search entire worktree", run: () => searchInFolder("") }] : []),
    ];
  };
  const searchActions = (match: FileSearchResult) => {
    const { path, line, excerpt, columns = [], lineText } = match;
    const reference = matchReference(match);
    const isSource = !markdownPath(path);
    return [[
      { label: "Open at match", run: () => openSearchMatch(path, line, columns[0]) },
      { label: "Open to the side", run: () => openFile(path, line, true, columns[0]) },
      ...(isSource && columns.length > 1 ? [{ label: "Open at occurrence", items: columns.map((column) => ({
        label: `Column ${column}`, run: () => openSearchMatch(path, line, column),
      })) }] : []),
      { label: "Reveal in Explorer tree", run: () => revealFile(path) },
    ], [
      { label: "Copy match reference", run: () => menu.copy(reference, "Copied match reference") },
      ...(!simulated ? [{ label: "Copy OmniAgent link to match", run: () => menu.copyOmniAgentLink(path, line, columns[0]) }] : []),
      ...(lineText != null ? [{ label: "Copy matching line", run: () => menu.copy(lineText, "Copied matching line") }]
        : [{ label: "Copy match preview", run: () => menu.copy(excerpt, "Copied match preview") }]),
      ...(columns.length > 1 ? [{ label: "Copy visible occurrence references", run: () => menu.copy(columns.map((column) => matchReference(match, column)).join("\n"), "Copied occurrence references") }] : []),
    ], !simulated && excerpt ? [
      { label: "Add match to conversation…", run: () => conversationActions.startContextText({ title: `Search match in ${fileName(path)}`, reference, text: lineText ?? excerpt }) },
    ] : [], searchScopeActions(path)];
  };
  const searchFileActions = (matches: readonly FileSearchResult[]) => {
    const first = matches[0]!;
    const { path, line, columns } = first;
    return [[
      { label: "Open first match", run: () => openSearchMatch(path, line, columns?.[0]) },
      { label: "Open first match to the side", run: () => openFile(path, line, true, columns?.[0]) },
      { label: "Reveal in Explorer tree", run: () => revealFile(path) },
    ], [
      { label: "Copy relative path", run: () => menu.copy(path, "Copied relative path") },
      { label: "Copy visible results", run: () => menu.copy(matches.map((match) => `${matchReference(match)} ${match.lineText ?? match.excerpt}`).join("\n"), "Copied file search results") },
      { label: "Copy visible occurrence references", run: () => menu.copy(matches.flatMap((match) => match.columns?.length
        ? match.columns.map((column) => matchReference(match, column)) : [matchReference(match)]).join("\n"), "Copied occurrence references") },
    ], !simulated ? [
      { label: "Add file matches to conversation…", run: () => conversationActions.startContextText({ title: `Search matches in ${fileName(path)}`, reference: path,
        text: matches.map((match) => `${match.line}${match.columns?.[0] ? `:${match.columns[0]}` : ""} ${match.excerpt}`).join("\n") }) },
    ] : [], searchScopeActions(path)];
  };
  const simulateConflict = () => {
    if (!active || !buffer) return;
    const incoming = buffer.external ?? `// Revision ${Number(buffer.revision) + 1} from an external editor\n${buffer.saved}`;
    setExamples((before) => ({ ...before, [active]: { ...buffer, external: incoming } }));
    setStatus(`External changes need review in ${active}`);
    setDialog({ kind: "conflict", path: active });
  };

  return (
    <section ref={workspace} className="file-workspace" aria-label="IDE workspace" onKeyDown={(event) => {
      if (!event.ctrlKey && !event.metaKey) return;
      if (event.key.toLowerCase() === "s" && active && buffer) { event.preventDefault(); void save(active); }
      if (event.key.toLowerCase() === "p") { event.preventDefault(); setSidebar(true); setNavigator("search"); }
    }}>
      <FileSidebar name="IDE navigation" open={sidebar} drawer={sidebarDrawer} onClose={() => {
        setSidebar(false);
        requestAnimationFrame(() => sidebarToggle.current?.focus({ preventScroll: true }));
      }}>
        <div className="file-sidebar-heading"><FolderTree size={16} /><h2>Explorer</h2>
          <div className="file-toolbar">
            <button type="button" className="fw-icon" title="Files" aria-label="Files" aria-pressed={navigator === "files"} onClick={() => setNavigator("files")}><Files size={15} /></button>
            <button type="button" className="fw-icon" title="Search" aria-label="Search" aria-pressed={navigator === "search"}
              onClick={() => { setSearchScope(undefined); setNavigator("search"); }}><FileSearch2 size={15} /></button>
          </div>
        </div>
        <div className="file-sidebar-scroll">
          {navigator === "search" ? (simulated
            ? <ExampleFileSearch query={query} setQuery={setQuery} results={results} onOpen={(path, line, column) => { if (sidebarDrawer) setSidebar(false); openSearchMatch(path, line, column); }} actions={searchActions}
              fileActions={searchFileActions} scope={searchScope} onClear={() => setSearchScope(undefined)} />
            : <ProjectFileSearch checkoutId={checkout.checkoutId} scope={searchScope} onOpen={(path, line, column) => { if (sidebarDrawer) setSidebar(false); openSearchMatch(path, line, column); }} actions={searchActions}
              fileActions={searchFileActions} onClear={() => setSearchScope(undefined)} />)
            : <><p className="file-sidebar-note" title={simulated ? undefined : checkout.checkoutRoot}>
              {simulated ? "EXAMPLE WORKTREE" : fileName(checkout.checkoutRoot.replaceAll("\\", "/"))}</p>
              <FileTree paths={paths} folders={treeActions.folders} active={active} onOpen={chooseFile} checkoutId={checkout.checkoutId} workspace="ide" simulated={simulated} reveal={reveal} actions={(target, selection) => selection ? [
                [
                  { label: "Open selected files", disabled: !selection.some(({ kind }) => kind === "file"),
                    run: () => { void openSelected(selection.filter(({ kind }) => kind === "file").map(({ path }) => path)); } },
                  ...comparison.selectionActions(selection),
                  { label: "Copy relative paths", run: () => menu.copy(selection.map(({ path }) => path).join("\n"), "Copied relative paths") },
                ],
                ...conversationActions.actions(target, selection), ...fileOperations.actions(target, selection),
              ] : [
                ...treeActions.actions(target),
                ...(target.kind !== "file" ? [[{ label: "Find in folder", run: () => searchInFolder(target.path) }]] : []),
                ...(target.kind === "file" ? [[
                  { label: "Open", run: () => openFile(target.path) },
                  ...(!markdownPath(target.path) ? [{ label: "Open to the side", run: () => openFile(target.path, undefined, true) }] : []),
                  { label: "Open in new window", run: () => openFile(target.path, undefined, false, undefined, true) },
                  ...(!simulated && markdownPath(target.path) ? [{ label: "Rename note…", run: () => renameNote(target.path) }] : []),
                  ...(!simulated ? [{ label: "Open working diff", run: () => openWorkspace("git", workingDiffTarget(target.path)) }] : []),
                  ...(!simulated && files[target.path] ? [{ label: "Save a copy…", run: () => treeActions.saveCopy(target.path, files[target.path]!.content) }] : []),
                ]] : []),
                ...(!simulated ? [[{ label: "Open terminal here", run: () => openTerminal(target.kind === "file" ? containingFolder(target.path) : target.path) }]] : []),
                ...menu.location(target),
                ...(target.kind === "file" ? [comparison.actions(target.path)] : []),
                ...conversationActions.actions(target),
                ...fileOperations.actions(target),
              ]} /></>}
        </div>
      </FileSidebar>
      <div className="file-workspace-main">
        <header className="file-workspace-header">
          <SidebarToggle open={sidebar} setOpen={setSidebar} label="Toggle IDE navigation" buttonRef={sidebarToggle} />
          <Code2 size={17} /><h1>IDE</h1>
          <div className="file-toolbar">
            <ControlTooltip label="Save file" shortcut="Ctrl+S"><button type="button" className="fw-icon" aria-label="Save file" disabled={!active || !dirty(active)} onClick={() => { if (active) void save(active); }}><Save size={16} aria-hidden="true" /></button></ControlTooltip>
            <button type="button" className="fw-icon" aria-label="Split editor" title="Split editor" disabled={!buffer} onClick={() => { if (active) dock.split(active); }}><SplitSquareHorizontal size={15} /></button>
            <button type="button" className="fw-icon" aria-label="Open changes" title="Open changes" onClick={() => openChanges("working")}><GitCompareArrows size={15} /></button>
            <button type="button" className="fw-icon" aria-label="Toggle bottom panel" title="Toggle bottom panel" aria-expanded={Boolean(bottom)} onClick={() => setBottom(bottom ? null : "terminal")}><PanelBottom size={15} /></button>
          </div>
        </header>

        {dock.error && <p role="alert" className="file-status"><AlertCircle size={15} />{dock.error}</p>}
        {(problem || source.error) && <p role="alert" className="file-status"><AlertCircle size={15} />{problem || source.error}</p>}
        {status && <p role="status" className="file-status">{status}</p>}

        <div className="file-dock-body">
          <FileDock onReady={dock.onReady} context={{
            renderFile: (path, _view, panelId) => {
              const entry = files[path];
              const failure = source.failures[path];
              if (!entry) return simulated || failure
                ? <FileUnavailable title="File unavailable" path={path} detail={failure} />
                : <p role="status" className="file-help">Loading {path}…</p>;
              const sourceProblem = source.files[path]?.sourceProblem;
              return (
                <div className="ide-file-view">
                  <FileCaption path={path} revision={entry.revision} onReveal={revealFile} actions={(item) => [
                    [{ label: "Reveal in Explorer tree", run: () => revealFile(item.path) },
                      ...(item.kind === "folder" ? [{ label: "Find in folder", run: () => searchInFolder(item.path) }] : []),
                      ...(!simulated ? [{ label: "Open terminal here", run: () => openTerminal(item.kind === "file" ? containingFolder(item.path) : item.path) }] : [])],
                    ...menu.location(item),
                  ]} />
                  {sourceProblem && <p role="alert" className="file-help">{sourceProblem}</p>}
                  <Suspense fallback={<p role="status" className="file-help">Loading source editor…</p>}>
                    <Editor scope={scope} path={path} content={entry.content} models={models} update={update} problems={report} jump={jumps[path]}
                      quickFix={quickFix?.panelId === panelId && dock.api?.activePanel?.id === panelId ? quickFix : undefined}
                      onQuickFixShown={finishQuickFix} onSettings={reportEditorSettings}
                      onCopy={menu.copy} onExcerpt={simulated ? undefined : conversationActions.startExcerpt}
                      onOmniAgentLink={simulated ? undefined : menu.copyOmniAgentLink}
                      onPermalink={simulated ? undefined : menu.copyPermalink} blameCheckoutId={simulated ? undefined : checkout.checkoutId} />
                  </Suspense>
                </div>
              );
            },
            empty: <FileUnavailable title="Open a source file" detail="Choose a file in the Explorer."
              onSimulate={simulated ? undefined : () => startSimulation()} />,
            dirty,
            requestClose,
            layoutActions: (api) => dock.layoutActions(api, dockCommands),
            groupActions: (id) => dock.groupActions(id, dockCommands),
            tabActions: (path, view, api) => [[
              { label: "Save", disabled: !dirty(path), run: () => { void save(path); } },
              ...(!simulated && files[path] ? [{ label: "Save a copy…", run: () => treeActions.saveCopy(path, files[path]!.content) }] : []),
              ...(files[path]?.external !== undefined ? [{ label: "Review external change", run: () => setDialog({ kind: "conflict", path }) }] : []),
              { label: "Split right", run: () => dock.split(path, view, "right", api.id) },
              { label: "Split down", run: () => dock.split(path, view, "below", api.id) },
              { label: "Reveal in Explorer tree", run: () => revealFile(path) },
              ...(!simulated ? [{ label: "Open working diff", run: () => openWorkspace("git", workingDiffTarget(path)) }] : []),
            ], ...menu.location({ kind: "file", path }), comparison.actions(path, api), ...conversationActions.actions({ kind: "file", path }), ...fileOperations.actions({ kind: "file", path }), [
              { label: "Close tab", run: () => requestClose(path, api) },
            ]],
          }} />
        </div>

        <ResizablePanel className="ide-bottom" hidden={!bottom} edge="top" resizeKey="ide-bottom" resizeLabel="IDE bottom panel">
          <div className="ide-bottom-toolbar">
            <button type="button" className="fw-btn" aria-pressed={bottom === "terminal"} onClick={() => setBottom("terminal")}><TerminalSquare size={14} /> Terminal</button>
            <button type="button" className="fw-btn" aria-pressed={bottom === "problems"} onClick={() => setBottom("problems")}><CircleAlert size={14} /> Problems{markerEntries.length ? ` (${markerEntries.length})` : ""}</button>
            {bottom === "terminal" && !simulated && terminals.panes.length > 1 && (
              <select aria-label="Terminal session" className="delivery-filter" value={terminals.active} onChange={(event) => terminals.select(event.currentTarget.value)}>
                {terminals.panes.map(({ id, title }) => <option key={id} value={id}>{title}</option>)}
              </select>
            )}
            <div className="file-toolbar">
              <button type="button" className="fw-icon" title="Clear terminal" aria-label="Clear terminal" disabled={bottom !== "terminal"}
                onClick={() => simulated ? setExampleClearRevision((previous) => previous + 1) : terminals.clear()}><Trash2 size={14} /></button>
            </div>
          </div>
          <div className="ide-bottom-body">
            <div className="ide-terminal-container" hidden={bottom !== "terminal"}>
              <Suspense fallback={<p className="file-help">Loading terminal…</p>}>
                {simulated ? <Terminal clearRevision={exampleClearRevision} /> : <div className="ide-terminal-panes">
                  {terminals.panes.map(({ id, directory, title, clearRevision, autoStart, generation, reviewedInput }) => {
                    const order = terminals.visible.indexOf(id);
                    const visible = bottom === "terminal" && order >= 0;
                    const active = visible && terminals.active === id;
                    const sessionActions: MenuGroups = [[
                      { label: "Rename terminal…", run: () => setTerminalRename({ id, title }) },
                      { label: "Duplicate terminal", run: () => terminals.duplicate(id, false) },
                      { label: "Split terminal", run: () => terminals.duplicate(id, true) },
                      { label: "Close terminal session", run: () => terminals.close(id) },
                    ]];
                    return <div key={`${id}:${generation ?? 0}`} className="ide-terminal-pane" hidden={!visible} data-side={order > 0 ? "right" : undefined} style={{ order }}>
                      <ContextActions className="ide-terminal-title" label={`${title} terminal`} groups={sessionActions}>
                        <button className="fw-btn" type="button" aria-pressed={active} title={directory || "Checkout root"} onClick={() => terminals.select(id)}>{title}</button>
                      </ContextActions>
                      <WorktreeTerminal checkoutId={checkout.checkoutId} active={active} visible={visible} sessionActions={sessionActions}
                        {...(reviewedInput === undefined ? {} : { reviewedInput })}
                        onFocus={() => terminals.select(id)} onOpenFile={(path, line, column) => openFile(path, line, false, column)}
                        clearRevision={clearRevision} onContextText={(text) => conversationActions.startContextText({
                          title: "PowerShell output", reference: `PowerShell · ${title}`, text,
                        })} {...(directory ? { relativeDirectory: directory } : {})} {...(autoStart ? { autoStart: true } : {})} />
                    </div>;
                  })}
                </div>}
              </Suspense>
            </div>
            {bottom === "problems" && <IdeProblems issues={markerEntries} simulated={simulated}
              open={({ path, line, column }, split) => openFile(path, line, split, column)} reveal={revealFile} quickFix={showQuickFix} copy={menu.copy}
              onContext={simulated ? undefined : (issue, instruction) => conversationActions.startContextText({
                title: `${issue.label} in ${fileName(issue.path)}`, reference: problemReference(issue),
                text: `${issue.label}: ${issue.message}`,
              }, instruction)} />}
          </div>
        </ResizablePanel>
        {active && buffer && <footer className="ide-statusbar">
          <EditorStatus path={active} saved={!dirty(active)} settings={editorSettings[active]} format={buffer.format} copy={menu.copy}
            canConvert={!dirty(active) && !source.files[active]?.sourceProblem && !source.files[active]?.draftRevision}
            onChangeFormat={!simulated && buffer.format ? (change) => setDialog({ kind: "format", path: active, change }) : undefined} />
          {buffer.external !== undefined && <button type="button" className="fw-btn" onClick={() => setDialog({ kind: "conflict", path: active })}>Review external change</button>}
          {simulated && buffer.external === undefined && <button type="button" className="fw-btn" onClick={simulateConflict}>Simulate external change</button>}
        </footer>}
      </div>

      {treeActions.dialog}
      {conversationActions.dialog}
      {fileOperations.dialog}
      {comparison.dialog}
      {dialog?.kind === "format" && <SourceFormatDialog path={dialog.path} change={dialog.change}
        preview={source.previewFormat} save={source.saveFormat} onClose={() => setDialog(undefined)} onSaved={() => {
          setStatus(`Converted and saved ${dialog.path}`);
          setDialog(undefined);
        }} />}
      {terminalRename && <FileDialog title="Rename terminal" description="Choose a name for this terminal session." onClose={() => setTerminalRename(undefined)}>
        <form onSubmit={(event) => {
          event.preventDefault();
          const title = terminalRename.title.trim();
          if (!title) return;
          terminals.rename(terminalRename.id, title);
          setTerminalRename(undefined);
        }}>
          <label className="file-field">Terminal name<input autoFocus value={terminalRename.title} maxLength={80}
            onChange={(event) => setTerminalRename({ id: terminalRename.id, title: event.currentTarget.value })} /></label>
          <div className="file-dialog-footer"><button type="submit" className="fw-btn fw-primary" disabled={!terminalRename.title.trim()}>Rename</button></div>
        </form>
      </FileDialog>}
      {dialog?.kind === "close" && <FileDialog title="Save changes?" description={`${dialog.path} has unsaved changes.`} onClose={cancelClose}
        ownerPanel={dialog.api}>
        <div className="file-dialog-footer">
          <button type="button" className="fw-btn" disabled={confirmingClose} onClick={cancelClose}>Cancel</button>
          <button type="button" className="fw-btn fw-destructive" disabled={confirmingClose} onClick={() => void confirmClose(dialog, false)}>Discard changes</button>
          <button type="button" className="fw-btn fw-primary" disabled={confirmingClose} onClick={() => void confirmClose(dialog, true)}>Save and close</button>
        </div>
      </FileDialog>}
      {dialog?.kind === "conflict" && <FileDialog title="File changed externally" description={`Review both versions of ${dialog.path} before continuing.`} onClose={() => setDialog(undefined)}>
        <div className="file-dialog-comparison">
          <div><h3>Local buffer</h3><pre>{files[dialog.path]?.content}</pre></div>
          <div><h3>External version</h3><pre>{files[dialog.path]?.external}</pre></div>
        </div>
        <div className="file-dialog-footer">
          <button type="button" className="fw-btn" onClick={() => setDialog(undefined)}>Review later</button>
          <button type="button" className="fw-btn fw-outlined" onClick={async () => {
            if (await resolve(dialog.path, false)) {
              setStatus(`Loaded external revision of ${dialog.path}`);
              setDialog(undefined);
            }
          }}>Use external version</button>
          <button type="button" className="fw-btn fw-primary" onClick={async () => {
            if (await resolve(dialog.path, true)) {
              setStatus(`Local edits retained against the external revision of ${dialog.path}`);
              setDialog(undefined);
            }
          }}>Keep local edits</button>
        </div>
      </FileDialog>}
    </section>
  );
}
