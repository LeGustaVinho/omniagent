import {
  AlertCircle, CircleAlert, Code2, FileSearch2, Files, FolderTree, GitCompareArrows,
  PanelBottom, Save, SplitSquareHorizontal, TerminalSquare, Trash2,
} from "lucide-react";
import { lazy, Suspense, useCallback, useEffect, useRef, useState } from "react";
import {
  FileDialog, FileDock, FileSearch, FileSidebar, FileTree, FileUnavailable,
  keepOnlyCleanClose, SidebarToggle, useFileDock, type FilePanelApi,
} from "@/ide-file-workspace";
import { exampleNotes, exampleSources, fileName, findFileText, normalizeRepositoryPath } from "@/ide-knowledge-data";
import type { CodeModel, CodeProblem } from "@/ide-code-editor";
import type { WorkspaceProps } from "@/workbench-contract";

const Editor = lazy(() => import("@/ide-code-editor").then(({ CodeEditor }) => ({ default: CodeEditor })));
const Terminal = lazy(() => import("@/ide-terminal").then(({ ExampleTerminal }) => ({ default: ExampleTerminal })));

interface Buffer {
  body: string;
  saved: string;
  revision: number;
  external?: string;
}

type IdeDialog =
  | { kind: "close"; path: string; api: FilePanelApi }
  | { kind: "conflict"; path: string; incoming: string };

function newBuffers(simulated: boolean, recovery: boolean): Record<string, Buffer> {
  return Object.fromEntries((simulated ? exampleSources : []).map(({ path, content }) => [
    path,
    { body: recovery && path === "src/health.ts" ? content + "\n// Recovered readiness checks\n" : content, saved: content, revision: 1 },
  ]));
}

export default function IdeWorkspace(props: WorkspaceProps) {
  const scope = `${props.checkout.checkoutId}:${props.simulation?.epoch ?? "real"}`;
  return <IdeSurface key={scope} {...props} scope={scope} />;
}

function IdeSurface({ checkout, target, simulation, startSimulation, openWorkspace, openChanges, scope }: WorkspaceProps & { scope: string }) {
  const simulated = Boolean(simulation);
  const firstPath = target && !target.toLowerCase().endsWith(".md") ? normalizeRepositoryPath(target) : simulated ? "src/health.ts" : undefined;
  const dock = useFileDock({ checkoutId: checkout.checkoutId, workspace: "ide", simulated, initialPath: firstPath });
  const [sidebar, setSidebar] = useState(() => window.innerWidth >= 960);
  const [navigator, setNavigator] = useState<"files" | "search">("files");
  const [query, setQuery] = useState("");
  const [files, setFiles] = useState(() => newBuffers(simulated, simulation?.scenario === "recovery"));
  const [jumps, setJumps] = useState<Record<string, { line: number; token: number }>>({});
  const [markers, setMarkers] = useState<Record<string, CodeProblem[]>>({});
  const [bottom, setBottom] = useState<"terminal" | "problems" | null>("terminal");
  const [terminalHeight, setTerminalHeight] = useState(34);
  const [clearRevision, setClearRevision] = useState(0);
  const [status, setStatus] = useState(simulation?.scenario === "recovery" ? "Recovered unsaved changes in src/health.ts" : "");
  const [dialog, setDialog] = useState<IdeDialog>();
  const models = useRef(new Map<string, CodeModel>()).current;
  const lastTarget = useRef(firstPath);
  const active = dock.path;
  const buffer = active ? files[active] : undefined;
  const dirty = (path: string) => {
    const entry = files[path];
    return Boolean(entry && (entry.external !== undefined || entry.body !== entry.saved));
  };
  const filesForSearch = [
    ...Object.entries(files).map(([path, value]) => ({ path, content: value.body })),
    ...(simulated ? exampleNotes : []),
  ];
  const results = findFileText(filesForSearch, query, true);
  const markerEntries = Object.entries(markers).flatMap(([path, items]) => items.map((item) => ({ path, ...item })));

  useEffect(() => () => {
    for (const { onChange, model } of models.values()) {
      onChange.dispose();
      model.dispose();
    }
    models.clear();
  }, [models]);

  const openFile = useCallback((path: string, line?: number) => {
    if (path.toLowerCase().endsWith(".md")) {
      openWorkspace("knowledge", path);
    } else {
      if (line !== undefined) setJumps((before) => ({ ...before, [path]: { line, token: (before[path]?.token ?? 0) + 1 } }));
      dock.open(path);
    }
  }, [dock.open, openWorkspace]);

  useEffect(() => {
    if (!target || !dock.ready) return;
    const next = normalizeRepositoryPath(target);
    if (next && next !== lastTarget.current) {
      lastTarget.current = next;
      openFile(next);
    }
  }, [dock.ready, openFile, target]);

  const update = useCallback((path: string, body: string) => {
    setFiles((before) => {
      const entry = before[path];
      return entry && entry.body !== body ? { ...before, [path]: { ...entry, body } } : before;
    });
  }, []);
  const report = useCallback((path: string, issues: CodeProblem[]) => setMarkers((before) => ({ ...before, [path]: issues })), []);

  const save = (path: string) => {
    const external = files[path]?.external;
    if (external !== undefined) {
      setDialog({ kind: "conflict", path, incoming: external });
      return false;
    }
    setFiles((before) => {
      const current = before[path];
      if (!current) return before;
      return { ...before, [path]: { body: current.body, saved: current.body, revision: current.revision + 1 } };
    });
    setStatus(`Saved ${path}`);
    return true;
  };
  const replaceModelBuffer = (path: string, content: string) => {
    const record = models.get(path);
    if (!record || record.model.getValue() === content) return;
    record.synchronizing = true;
    try { record.model.setValue(content); } finally { record.synchronizing = false; }
  };
  const discard = (path: string) => {
    const entry = files[path];
    if (!entry) return;
    const body = entry.external ?? entry.saved;
    replaceModelBuffer(path, body);
    setFiles((before) => ({ ...before, [path]: { body, saved: body, revision: entry.revision + Number(entry.external !== undefined) } }));
  };
  const requestClose = (path: string, api: FilePanelApi) => {
    if (!dirty(path) || !dock.api || keepOnlyCleanClose(dock.api, path, api)) api.close();
    else setDialog({ kind: "close", path, api });
  };
  const simulateConflict = () => {
    if (!active || !buffer) return;
    const incoming = buffer.external ?? `// Revision ${buffer.revision + 1} from an external editor\n${buffer.saved}`;
    setFiles((before) => ({ ...before, [active]: { ...buffer, external: incoming } }));
    setStatus(`External changes need review in ${active}`);
    setDialog({ kind: "conflict", path: active, incoming });
  };

  return (
    <section className="file-workspace" aria-label="IDE workspace" onKeyDown={(event) => {
      if (!event.ctrlKey && !event.metaKey) return;
      if (event.key.toLowerCase() === "s" && active && buffer) { event.preventDefault(); save(active); }
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
          {navigator === "search" ? <FileSearch query={query} setQuery={setQuery} results={results} onOpen={openFile} placeholder="Search files and content" unavailable={!simulated} />
            : simulated ? <><p className="file-sidebar-note">EXAMPLE WORKTREE</p><FileTree paths={filesForSearch.map(({ path }) => path)} active={active} onOpen={openFile} /></>
              : <p className="file-sidebar-note">The file service is pending for this checkout.</p>}
        </div>
      </FileSidebar>}
      <div className="file-workspace-main">
        <header className="file-workspace-header">
          <SidebarToggle open={sidebar} setOpen={setSidebar} label="Toggle IDE navigation" />
          <Code2 size={17} /><h1>IDE</h1>
          <div className="file-toolbar">
            <button type="button" className="fw-icon" aria-label="Save file" title="Save file (Ctrl+S)" disabled={!active || !dirty(active)} onClick={() => { if (active) save(active); }}><Save size={15} /></button>
            <button type="button" className="fw-icon" aria-label="Split editor" title="Split editor" disabled={!buffer} onClick={() => { if (active) dock.split(active); }}><SplitSquareHorizontal size={15} /></button>
            <button type="button" className="fw-icon" aria-label="Open changes" title="Open changes" onClick={() => openChanges("working")}><GitCompareArrows size={15} /></button>
            <button type="button" className="fw-icon" aria-label="Toggle bottom panel" title="Toggle bottom panel" aria-expanded={Boolean(bottom)} onClick={() => setBottom(bottom ? null : "terminal")}><PanelBottom size={15} /></button>
          </div>
        </header>

        {dock.error && <p role="alert" className="file-status"><AlertCircle size={15} />{dock.error}</p>}
        {status && <p role="status" className="file-status">{status}</p>}

        <div className="file-dock-body">
          <FileDock onReady={dock.onReady} context={{
            renderFile: (path) => {
              const entry = files[path];
              if (!entry) return <FileUnavailable title="File unavailable" detail={simulated ? path : `${path} · repository file integration is pending.`} />;
              return (
                <div className="ide-file-view">
                  <div className="ide-file-caption">
                    <span>{path.replaceAll("/", " / ")}</span>
                    <span>r{entry.revision}</span>
                  </div>
                  <Suspense fallback={<p role="status" className="file-help">Loading source editor…</p>}>
                    <Editor scope={scope} path={path} content={entry.body} models={models} update={update} problems={report} jump={jumps[path]} />
                  </Suspense>
                </div>
              );
            },
            empty: <FileUnavailable title={simulated ? "Open a source file" : "File integration pending"}
              detail={simulated ? "Choose a file in the Explorer or search by path and content." : "Use the simulation to explore the editor, terminal and worktree recovery."}
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
          {simulated ? <div className="ide-bottom-body">
            <div className="ide-terminal-container" hidden={bottom !== "terminal"}>
              <Suspense fallback={<p className="file-help">Loading terminal…</p>}><Terminal clearRevision={clearRevision} /></Suspense>
            </div>
            {bottom === "problems" && <ul className="ide-problems">
              {markerEntries.length ? markerEntries.map(({ path, label, message, line, column }, index) => (
                <li key={`${path}:${line}:${column}:${index}`}>
                  <button type="button" onClick={() => openFile(path, line)}>
                    <CircleAlert size={15} /><span>{label} · {message}</span><small>{fileName(path)}:{line}:{column}</small>
                  </button>
                </li>
              )) : <li className="file-help">No editor diagnostics in this workspace.</li>}
              <li className="file-help">ripgrep missing · Configure its executable in Settings to enable live repository search.</li>
            </ul>}
          </div> : <p className="file-sidebar-note">{bottom === "terminal" ? "PowerShell terminal integration is pending." : "Diagnostics appear when a connected editor is open."}</p>}
        </div>}
        {simulated && active && buffer && <footer className="ide-statusbar">
          <span>{buffer.body === buffer.saved ? "Saved" : "Unsaved changes"} · UTF-8</span>
          <button type="button" className="fw-btn" onClick={simulateConflict}>{buffer.external === undefined ? "Simulate external change" : "Review external change"}</button>
        </footer>}
      </div>

      {dialog?.kind === "close" && <FileDialog title="Save changes?" description={`${dialog.path} has unsaved changes.`} onClose={() => setDialog(undefined)}>
        <div className="file-dialog-footer">
          <button type="button" className="fw-btn" onClick={() => setDialog(undefined)}>Cancel</button>
          <button type="button" className="fw-btn fw-destructive" onClick={() => { discard(dialog.path); dialog.api.close(); setDialog(undefined); }}>Discard changes</button>
          <button type="button" className="fw-btn fw-primary" onClick={() => { if (save(dialog.path)) { dialog.api.close(); setDialog(undefined); } }}>Save and close</button>
        </div>
      </FileDialog>}
      {dialog?.kind === "conflict" && <FileDialog title="File changed externally" description={`Review both versions of ${dialog.path} before continuing.`} onClose={() => setDialog(undefined)}>
        <div className="file-dialog-comparison">
          <div><h3>Local buffer</h3><pre>{files[dialog.path]?.body}</pre></div>
          <div><h3>External version</h3><pre>{dialog.incoming}</pre></div>
        </div>
        <div className="file-dialog-footer">
          <button type="button" className="fw-btn" onClick={() => setDialog(undefined)}>Review later</button>
          <button type="button" className="fw-btn fw-outlined" onClick={() => {
            const { path, incoming } = dialog;
            replaceModelBuffer(path, incoming);
            setFiles((before) => ({ ...before, [path]: { body: incoming, saved: incoming, revision: (before[path]?.revision ?? 0) + 1 } }));
            setStatus(`Loaded external revision of ${path}`);
            setDialog(undefined);
          }}>Use external version</button>
          <button type="button" className="fw-btn fw-primary" onClick={() => {
            const { path, incoming } = dialog;
            setFiles((before) => {
              const current = before[path];
              return current ? { ...before, [path]: { ...current, external: undefined, saved: incoming, revision: current.revision + 1 } } : before;
            });
            setStatus(`Local edits retained against the external revision of ${path}`);
            setDialog(undefined);
          }}>Keep local edits</button>
        </div>
      </FileDialog>}
    </section>
  );
}
