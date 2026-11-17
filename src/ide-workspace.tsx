import {
  AlertCircle, CircleAlert, Code2, FileSearch2, Files, FolderTree, GitCompareArrows,
  PanelBottom, Save, SplitSquareHorizontal, TerminalSquare, Trash2,
} from "lucide-react";
import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import {
  FileDialog, FileDock, FileSearch, FileSidebar, FileTree, FileUnavailable,
  keepOnlyCleanClose, SidebarToggle, useFileDock, useFileSidebar, type FilePanelApi,
} from "@/ide-file-workspace";
import { noteSearchTarget, ProjectFileSearch } from "@/ide-file-search";
import { exampleNotes, exampleSources, fileName, findFileText, normalizeRepositoryPath } from "@/ide-knowledge-data";
import type { CodeModel, CodeProblem } from "@/ide-code-editor";
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
  const firstPath = simulated ? target && !markdownPath(target) ? normalizeRepositoryPath(target) : "src/health.ts" : undefined;
  const dock = useFileDock({ checkoutId: checkout.checkoutId, workspace: "ide", simulated, initialPath: firstPath });
  const source = useRepositorySourceBuffers(checkout.checkoutId, !simulated);
  const { load: loadSource, watch: watchSource, update: updateSource } = source;
  const [sidebar, setSidebar] = useFileSidebar();
  const [navigator, setNavigator] = useState<"files" | "search">("files");
  const [query, setQuery] = useState("");
  const [examples, setExamples] = useState(() => newBuffers(simulated, simulation?.scenario === "recovery"));
  const files: Record<string, NoteBuffer> = simulated ? examples : source.files;
  const [jumps, setJumps] = useState<Record<string, { line: number; token: number }>>({});
  const [markers, setMarkers] = useState<Record<string, CodeProblem[]>>({});
  const [bottom, setBottom] = useState<"terminal" | "problems" | null>("terminal");
  const [terminalHeight, setTerminalHeight] = useState(34);
  const [clearRevision, setClearRevision] = useState(0);
  const [status, setStatus] = useState(simulation?.scenario === "recovery" ? "Recovered unsaved changes in src/health.ts" : "");
  const [problem, setProblem] = useState("");
  const [dialog, setDialog] = useState<IdeDialog>();
  const models = useRef(new Map<string, CodeModel>()).current;
  const lastTarget = useRef(firstPath);
  const openRequest = useRef(0);
  const active = dock.path;
  const buffer = active ? files[active] : undefined;
  const dirty = (path: string) => {
    const entry = files[path];
    return Boolean(entry && (entry.external !== undefined || entry.content !== entry.saved));
  };
  const filesForSearch = [
    ...Object.entries(examples).map(([path, value]) => ({ path, content: value.content })),
    ...(simulated ? exampleNotes : []),
  ];
  const results = findFileText(filesForSearch, query, true);
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

  const openFile = useCallback((path: string, line?: number) => {
    const request = ++openRequest.current;
    if (markdownPath(path)) {
      openWorkspace("knowledge", !simulated && line ? noteSearchTarget(path, line, request) : path);
      return;
    }
    const focus = (file: string) => {
      if (line !== undefined) setJumps((before) => ({ ...before, [file]: { line, token: (before[file]?.token ?? 0) + 1 } }));
      dock.open(file);
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
  }, [dock.open, loadSource, openWorkspace, simulated, watchSource]);

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
    const next = normalizeRepositoryPath(target);
    if (next && next !== lastTarget.current) {
      lastTarget.current = next;
      openFile(next);
    }
  }, [dock.ready, openFile, target]);

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
      setDialog({ kind: "conflict", path });
      return false;
    }
    setProblem("");
    if (!simulated) {
      source.clearError();
      if (!await source.save(path)) {
        if (source.getFile(path)?.external !== undefined) setDialog({ kind: "conflict", path });
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
  const closeView = (path: string, api: FilePanelApi) => {
    const remains = dock.api && keepOnlyCleanClose(dock.api, path, api);
    api.close();
    if (remains) return;
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
  const requestClose = (path: string, api: FilePanelApi) => {
    if (!dirty(path) || !dock.api || keepOnlyCleanClose(dock.api, path, api)) closeView(path, api);
    else setDialog({ kind: "close", path, api });
  };
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
            <button type="button" className="fw-icon" title="Search" aria-label="Search" aria-pressed={navigator === "search"} onClick={() => setNavigator("search")}><FileSearch2 size={15} /></button>
          </div>
        </div>
        <div className="file-sidebar-scroll">
          {navigator === "search" ? (simulated
            ? <FileSearch query={query} setQuery={setQuery} results={results} onOpen={openFile} placeholder="Search files and content" />
            : <ProjectFileSearch checkoutId={checkout.checkoutId} onOpen={openFile} />)
            : <><p className="file-sidebar-note" title={simulated ? undefined : checkout.checkoutRoot}>
              {simulated ? "EXAMPLE WORKTREE" : fileName(checkout.checkoutRoot.replaceAll("\\", "/"))}</p>
              <FileTree paths={paths} active={active} onOpen={openFile} /></>}
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
                  <div className="ide-file-caption">
                    <span>{path.replaceAll("/", " / ")}</span>
                    <span>{typeof entry.revision === "number" ? `r${entry.revision}` : entry.revision}</span>
                  </div>
                  {sourceProblem && <p role="alert" className="file-help">{sourceProblem}</p>}
                  <Suspense fallback={<p role="status" className="file-help">Loading source editor…</p>}>
                    <Editor scope={scope} path={path} content={entry.content} models={models} update={update} problems={report} jump={jumps[path]} />
                  </Suspense>
                </div>
              );
            },
            empty: <FileUnavailable title="Open a source file" detail="Choose a file in the Explorer."
              onSimulate={simulated ? undefined : () => startSimulation()} />,
            dirty,
            requestClose,
          }} />
        </div>

        {bottom && <div className="ide-bottom" style={{ height: `${terminalHeight}%` }}>
          <div className="ide-bottom-toolbar">
            <button type="button" className="fw-btn" aria-pressed={bottom === "terminal"} onClick={() => setBottom("terminal")}><TerminalSquare size={14} /> Terminal</button>
            <button type="button" className="fw-btn" aria-pressed={bottom === "problems"} onClick={() => setBottom("problems")}><CircleAlert size={14} /> Problems{markerEntries.length ? ` (${markerEntries.length})` : ""}</button>
            <input aria-label="Resize bottom panel" title="Resize bottom panel" type="range" min={23} max={55} value={terminalHeight} onChange={(event) => setTerminalHeight(Number(event.target.value))} />
            <div className="file-toolbar">
              {simulated && <button type="button" className="fw-icon" title="Clear terminal" aria-label="Clear terminal" onClick={() => setClearRevision((value) => value + 1)}><Trash2 size={14} /></button>}
            </div>
          </div>
          <div className="ide-bottom-body">
            <div className="ide-terminal-container" hidden={bottom !== "terminal"}>
              <Suspense fallback={<p className="file-help">Loading terminal…</p>}>
                {simulated ? <Terminal clearRevision={clearRevision} />
                  : <WorktreeTerminal checkoutId={checkout.checkoutId} active={bottom === "terminal"} />}
              </Suspense>
            </div>
            {bottom === "problems" && <ul className="ide-problems">
              {markerEntries.length ? markerEntries.map(({ path, label, message, line, column }, index) => (
                <li key={`${path}:${line}:${column}:${index}`}>
                  <button type="button" onClick={() => openFile(path, line)}>
                    <CircleAlert size={15} /><span>{label} · {message}</span><small>{fileName(path)}:{line}:{column}</small>
                  </button>
                </li>
              )) : <li className="file-help">No editor diagnostics in this workspace.</li>}
              {simulated && <li className="file-help">ripgrep missing · Configure its executable in Settings to enable live repository search.</li>}
            </ul>}
          </div>
        </div>}
        {active && buffer && <footer className="ide-statusbar">
          <span>{dirty(active) ? "Unsaved changes" : "Saved"} · UTF-8</span>
          {buffer.external !== undefined && <button type="button" className="fw-btn" onClick={() => setDialog({ kind: "conflict", path: active })}>Review external change</button>}
          {simulated && buffer.external === undefined && <button type="button" className="fw-btn" onClick={simulateConflict}>Simulate external change</button>}
        </footer>}
      </div>

      {dialog?.kind === "close" && <FileDialog title="Save changes?" description={`${dialog.path} has unsaved changes.`} onClose={() => setDialog(undefined)}>
        <div className="file-dialog-footer">
          <button type="button" className="fw-btn" onClick={() => setDialog(undefined)}>Cancel</button>
          <button type="button" className="fw-btn fw-destructive" onClick={async () => {
            if (await resolve(dialog.path, false)) { closeView(dialog.path, dialog.api); setDialog(undefined); }
          }}>Discard changes</button>
          <button type="button" className="fw-btn fw-primary" onClick={async () => {
            if (await save(dialog.path)) { closeView(dialog.path, dialog.api); setDialog(undefined); }
          }}>Save and close</button>
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
