import {
  AlertCircle, CircleAlert, Code2, FileSearch2, Files, FolderTree, GitCompareArrows,
  PanelBottom, Save, SplitSquareHorizontal, TerminalSquare, Trash2,
} from "lucide-react";
import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import {
  FileCaption, FileDialog, FileDock, FileSidebar, FileTree, FileUnavailable,
  keepOnlyCleanClose, SidebarToggle, useFileDock, useFileSidebar, type FilePanelApi,
} from "@/ide-file-workspace";
import { ExampleFileSearch, noteSearchTarget, ProjectFileSearch, readFolderSearchTarget } from "@/ide-file-search";
import { containingFolder, fileContextActions, fileHistoryTarget, noteRenameTarget, openNotesTarget, readSourceLineTarget, readTerminalFolderTarget, workingDiffTarget } from "@/file-context-actions";
import { ContextActions } from "@/components/ui/context-actions";
import { useCheckoutTreeActions } from "@/checkout-tree-actions";
import { useCheckoutConversationActions } from "@/checkout-conversation-actions";
import { changedItemPath, itemIncludesPath, useCheckoutItemOperations } from "@/checkout-item-operations";
import { useCheckoutFileComparison } from "@/checkout-file-comparison";
import { exampleNotes, exampleSources, fileName, findFileText, normalizeRepositoryPath, type FileSearchResult } from "@/ide-knowledge-data";
import type { CodeModel, CodeProblem } from "@/ide-code-editor";
import { ResizablePanel } from "@/panel-resize";
import { useRepositorySourceBuffers, type NoteBuffer } from "@/repository-note-buffers";
import type { WorkspaceProps } from "@/workbench-contract";

const Editor = lazy(() => import("@/ide-code-editor").then(({ CodeEditor }) => ({ default: CodeEditor })));
const Terminal = lazy(() => import("@/ide-terminal").then(({ ExampleTerminal }) => ({ default: ExampleTerminal })));
const WorktreeTerminal = lazy(() => import("@/ide-terminal").then(({ ManualTerminal }) => ({ default: ManualTerminal })));

type IdeDialog =
  | { kind: "close"; path: string; api: FilePanelApi }
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
  const [sidebar, setSidebar] = useFileSidebar();
  const [navigator, setNavigator] = useState<"files" | "search">(requestedFolder === undefined ? "files" : "search");
  const [searchFolder, setSearchFolder] = useState(requestedFolder);
  const [reveal, setReveal] = useState<{ path: string }>();
  const [query, setQuery] = useState("");
  const [examples, setExamples] = useState(() => newBuffers(simulated, simulation?.scenario === "recovery"));
  const files: Record<string, NoteBuffer> = simulated ? examples : source.files;
  const [jumps, setJumps] = useState<Record<string, { line: number; column?: number; token: number }>>({});
  const [markers, setMarkers] = useState<Record<string, CodeProblem[]>>({});
  const [bottom, setBottom] = useState<"terminal" | "problems" | null>("terminal");
  const [terminalFolders, setTerminalFolders] = useState<string[]>([""]);
  const [terminalFolder, setTerminalFolder] = useState("");
  const [autoStartRoot, setAutoStartRoot] = useState(false);
  const [clearRevisions, setClearRevisions] = useState<Record<string, number>>({});
  const openTerminal = useCallback((folder: string) => {
    setBottom("terminal");
    setTerminalFolder(folder);
    if (!folder) setAutoStartRoot(true);
    setTerminalFolders((previous) => previous.includes(folder) ? previous : [...previous, folder]);
  }, []);
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
  const results = findFileText(searchFolder ? filesForSearch.filter(({ path }) => path.startsWith(`${searchFolder}/`)) : filesForSearch, query, true);
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

  const openFile = useCallback((path: string, line?: number, split = false, column?: number) => {
    const request = ++openRequest.current;
    if (markdownPath(path)) {
      openWorkspace("knowledge", !simulated && line ? noteSearchTarget(path, line, request) : path);
      return;
    }
    const focus = (file: string) => {
      if (line !== undefined) setJumps((before) => ({ ...before, [file]: { line, column, token: (before[file]?.token ?? 0) + 1 } }));
      if (split) dock.split(file);
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
  }, [dock.open, dock.split, loadSource, openWorkspace, simulated, watchSource]);

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
    if (requestedTerminal !== undefined) {
      openTerminal(requestedTerminal);
      return;
    }
    if (requestedFolder !== undefined) {
      setSidebar(true);
      setSearchFolder(requestedFolder);
      setNavigator("search");
      return;
    }
    const location = readSourceLineTarget(target);
    const next = normalizeRepositoryPath(location?.path ?? target);
    const request = location ? target : next;
    if (next && request !== lastTarget.current) {
      lastTarget.current = request;
      openFile(next, location?.line);
    }
  }, [dock.ready, openFile, openTerminal, requestedFolder, requestedTerminal, setSidebar, target]);

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
      source.clearError();
      if (!await source.save(path)) {
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
  const menu = fileContextActions({ checkout, simulated, status: setStatus, report: setProblem });
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
  const searchActions = ({ path, line, excerpt }: FileSearchResult) => [[
    { label: "Open at match", run: () => openFile(path, line) },
    ...(!markdownPath(path) ? [{ label: "Open to the side", run: () => openFile(path, line, true) }] : []),
    { label: "Reveal in Explorer tree", run: () => revealFile(path) },
  ], [
    { label: "Copy line reference", run: () => menu.copy(`${path}:${line}`, "Copied line reference") },
    { label: "Copy matched text", run: () => menu.copy(excerpt, "Copied matched text") },
  ], simulated || !excerpt ? [] : [
    { label: "Add match to conversation…", run: () => conversationActions.startExcerpt({ path, text: excerpt, startLine: line, endLine: line }) },
  ]];
  const simulateConflict = () => {
    if (!active || !buffer) return;
    const incoming = buffer.external ?? `// Revision ${Number(buffer.revision) + 1} from an external editor\n${buffer.saved}`;
    setExamples((before) => ({ ...before, [active]: { ...buffer, external: incoming } }));
    setStatus(`External changes need review in ${active}`);
    setDialog({ kind: "conflict", path: active });
  };

  return (
    <section className="file-workspace" aria-label="IDE workspace" onKeyDown={(event) => {
      if (!event.ctrlKey && !event.metaKey) return;
      if (event.key.toLowerCase() === "s" && active && buffer) { event.preventDefault(); void save(active); }
      if (event.key.toLowerCase() === "p") { event.preventDefault(); setSidebar(true); setNavigator("search"); }
    }}>
      {sidebar && <FileSidebar name="IDE navigation">
        <div className="file-sidebar-heading"><FolderTree size={16} /><h2>Explorer</h2>
          <div className="file-toolbar">
            <button type="button" className="fw-icon" title="Files" aria-label="Files" aria-pressed={navigator === "files"} onClick={() => setNavigator("files")}><Files size={15} /></button>
            <button type="button" className="fw-icon" title="Search" aria-label="Search" aria-pressed={navigator === "search"}
              onClick={() => { setSearchFolder(undefined); setNavigator("search"); }}><FileSearch2 size={15} /></button>
          </div>
        </div>
        <div className="file-sidebar-scroll">
          {navigator === "search" ? (simulated
            ? <ExampleFileSearch query={query} setQuery={setQuery} results={results} onOpen={openFile} actions={searchActions} />
            : <ProjectFileSearch checkoutId={checkout.checkoutId} folder={searchFolder} onOpen={openFile} actions={searchActions} />)
            : <><p className="file-sidebar-note" title={simulated ? undefined : checkout.checkoutRoot}>
              {simulated ? "EXAMPLE WORKTREE" : fileName(checkout.checkoutRoot.replaceAll("\\", "/"))}</p>
              <FileTree paths={paths} folders={treeActions.folders} active={active} onOpen={openFile} checkoutId={checkout.checkoutId} workspace="ide" simulated={simulated} reveal={reveal} actions={(target, selection) => selection ? [
                [
                  { label: "Open selected files", disabled: !selection.some(({ kind }) => kind === "file"),
                    run: () => { void openSelected(selection.filter(({ kind }) => kind === "file").map(({ path }) => path)); } },
                  ...comparison.selectionActions(selection),
                  { label: "Copy relative paths", run: () => menu.copy(selection.map(({ path }) => path).join("\n"), "Copied relative paths") },
                ],
                ...conversationActions.actions(target, selection), ...fileOperations.actions(target, selection),
              ] : [
                ...treeActions.actions(target),
                ...(target.kind !== "file" ? [[{ label: "Find in folder", run: () => {
                  setSearchFolder(target.path); setNavigator("search");
                } }]] : []),
                ...(target.kind === "file" ? [[
                  { label: "Open", run: () => openFile(target.path) },
                  ...(!markdownPath(target.path) ? [{ label: "Open to the side", run: () => openFile(target.path, undefined, true) }] : []),
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
      </FileSidebar>}
      <div className="file-workspace-main">
        <header className="file-workspace-header">
          <SidebarToggle open={sidebar} setOpen={setSidebar} label="Toggle IDE navigation" />
          <Code2 size={17} /><h1>IDE</h1>
          <div className="file-toolbar">
            <button type="button" className="fw-icon" aria-label="Save file" title="Save file (Ctrl+S)" disabled={!active || !dirty(active)} onClick={() => { if (active) void save(active); }}><Save size={15} /></button>
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
            renderFile: (path) => {
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
                      ...(item.kind === "folder" ? [{ label: "Find in folder", run: () => {
                        setSidebar(true); setSearchFolder(item.path); setNavigator("search");
                      } }] : []),
                      ...(!simulated ? [{ label: "Open terminal here", run: () => openTerminal(item.kind === "file" ? containingFolder(item.path) : item.path) }] : [])],
                    ...menu.location(item),
                  ]} />
                  {sourceProblem && <p role="alert" className="file-help">{sourceProblem}</p>}
                  <Suspense fallback={<p role="status" className="file-help">Loading source editor…</p>}>
                    <Editor scope={scope} path={path} content={entry.content} models={models} update={update} problems={report} jump={jumps[path]}
                      onCopy={menu.copy} onExcerpt={simulated ? undefined : conversationActions.startExcerpt}
                      onPermalink={simulated ? undefined : menu.copyPermalink} />
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
            ], ...menu.location({ kind: "file", path }), comparison.actions(path), ...conversationActions.actions({ kind: "file", path }), ...fileOperations.actions({ kind: "file", path }), [
              { label: "Close tab", run: () => requestClose(path, api) },
            ]],
          }} />
        </div>

        {bottom && <ResizablePanel className="ide-bottom" edge="top" resizeKey="ide-bottom" resizeLabel="IDE bottom panel">
          <div className="ide-bottom-toolbar">
            <button type="button" className="fw-btn" aria-pressed={bottom === "terminal"} onClick={() => setBottom("terminal")}><TerminalSquare size={14} /> Terminal</button>
            <button type="button" className="fw-btn" aria-pressed={bottom === "problems"} onClick={() => setBottom("problems")}><CircleAlert size={14} /> Problems{markerEntries.length ? ` (${markerEntries.length})` : ""}</button>
            {bottom === "terminal" && !simulated && terminalFolders.length > 1 && <>
              <select aria-label="Terminal directory" className="delivery-filter" value={terminalFolder} onChange={(event) => setTerminalFolder(event.currentTarget.value)}>
                {terminalFolders.map((folder) => <option key={folder} value={folder}>{folder || "Checkout root"}</option>)}
              </select>
              {terminalFolder && <button type="button" className="fw-btn" onClick={() => {
                setTerminalFolders((previous) => previous.filter((folder) => folder !== terminalFolder));
                setTerminalFolder("");
              }}>Close folder terminal</button>}
            </>}
            <div className="file-toolbar">
              <button type="button" className="fw-icon" title="Clear terminal" aria-label="Clear terminal" disabled={bottom !== "terminal"}
                onClick={() => setClearRevisions((previous) => ({ ...previous, [terminalFolder]: (previous[terminalFolder] ?? 0) + 1 }))}><Trash2 size={14} /></button>
            </div>
          </div>
          <div className="ide-bottom-body">
            <div className="ide-terminal-container" hidden={bottom !== "terminal"}>
              <Suspense fallback={<p className="file-help">Loading terminal…</p>}>
                {simulated ? <Terminal clearRevision={clearRevisions[""] ?? 0} />
                  : terminalFolders.map((folder) => <div key={folder} className="h-full min-h-0" hidden={folder !== terminalFolder}>
                    <WorktreeTerminal checkoutId={checkout.checkoutId} active={bottom === "terminal" && folder === terminalFolder}
                      clearRevision={clearRevisions[folder] ?? 0} onContextText={(text) => conversationActions.startContextText({
                        title: "PowerShell output", reference: `PowerShell · ${folder || "Checkout root"}`, text,
                      })} {...(folder ? { relativeDirectory: folder, autoStart: true } : autoStartRoot ? { autoStart: true } : {})} />
                  </div>)}
              </Suspense>
            </div>
            {bottom === "problems" && <ul className="ide-problems">
              {markerEntries.length ? markerEntries.map(({ path, label, message, line, column }, index) => (
                <li key={`${path}:${line}:${column}:${index}`}>
                  <ContextActions label={`${path}:${line}:${column}`} groups={[[
                    { label: "Go to problem", run: () => openFile(path, line, false, column) },
                    { label: "Reveal in Explorer tree", run: () => revealFile(path) },
                  ], [
                    { label: "Copy diagnostic", run: () => menu.copy(`${path}:${line}:${column} ${label}: ${message}`, "Copied diagnostic") },
                    { label: "Copy location", run: () => menu.copy(`${path}:${line}:${column}`, "Copied problem location") },
                  ], simulated ? [] : [
                    { label: "Explain diagnostic…", run: () => conversationActions.startExcerpt({ path, startLine: line, endLine: line,
                      text: `${label} at column ${column}: ${message}` }, "Explain this diagnostic and suggest how to resolve it.") },
                  ]]}>
                    <button className="ide-problem-open" type="button" onClick={() => openFile(path, line, false, column)}>
                      <CircleAlert size={15} /><span>{label} · {message}</span><small>{fileName(path)}:{line}:{column}</small>
                    </button>
                  </ContextActions>
                </li>
              )) : <li className="file-help">No editor diagnostics in this workspace.</li>}
              {simulated && <li className="file-help">ripgrep missing · Configure its executable in Settings to enable live repository search.</li>}
            </ul>}
          </div>
        </ResizablePanel>}
        {active && buffer && <footer className="ide-statusbar">
          <span>{dirty(active) ? "Unsaved changes" : "Saved"} · UTF-8</span>
          {buffer.external !== undefined && <button type="button" className="fw-btn" onClick={() => setDialog({ kind: "conflict", path: active })}>Review external change</button>}
          {simulated && buffer.external === undefined && <button type="button" className="fw-btn" onClick={simulateConflict}>Simulate external change</button>}
        </footer>}
      </div>

      {treeActions.dialog}
      {conversationActions.dialog}
      {fileOperations.dialog}
      {comparison.dialog}
      {dialog?.kind === "close" && <FileDialog title="Save changes?" description={`${dialog.path} has unsaved changes.`} onClose={cancelClose}>
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
