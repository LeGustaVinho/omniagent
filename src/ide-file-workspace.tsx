import { invoke } from "@tauri-apps/api/core";
import {
  DockviewReact, themeDark, themeLight, type DockviewApi, type DockviewReadyEvent,
  type IDockviewPanelHeaderProps, type IDockviewPanelProps, type SerializedDockview,
} from "dockview-react";
import { FileCode2, FileText, FolderOpen, PanelLeftClose, X } from "lucide-react";
import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { Dialog } from "radix-ui";
import { fileName } from "@/ide-knowledge-data";
import "dockview/dist/styles/dockview.css";
import "./ide-knowledge.css";

export type FileView = "editor" | "reading";

interface FilePanelParams {
  path: string;
  view: FileView;
}

export type FilePanelApi = IDockviewPanelProps["api"];

export interface FileDockContextValue {
  renderFile: (path: string, view: FileView) => ReactNode;
  empty: ReactNode;
  dirty: (path: string) => boolean;
  requestClose: (path: string, api: FilePanelApi) => void;
}

const FileDockContext = createContext<FileDockContextValue | null>(null);

function FileDockPanel({ params }: IDockviewPanelProps<FilePanelParams>) {
  return useContext(FileDockContext)?.renderFile(params.path, params.view) ?? null;
}

function FileDockTab({ params, api }: IDockviewPanelHeaderProps<FilePanelParams>) {
  const context = useContext(FileDockContext);
  const title = fileName(params.path) + (params.view === "reading" ? " · Reading" : "");
  return (
    <div className="file-tab-label" title={params.path}>
      <span>{title}</span>
      {params.view === "editor" && context?.dirty(params.path) && <i aria-label="Unsaved changes" />}
      <button type="button" aria-label={`Close ${title}`} onPointerDown={(event) => event.stopPropagation()}
        onClick={(event) => { event.stopPropagation(); context?.requestClose(params.path, api); }}><X size={13} /></button>
    </div>
  );
}

function FileDockEmpty() {
  return <>{useContext(FileDockContext)?.empty}</>;
}

const filePanelComponents = { file: FileDockPanel };

export function FileDock({ context, onReady }: { context: FileDockContextValue; onReady: (event: DockviewReadyEvent) => void }) {
  const [light, setLight] = useState(false);
  useEffect(() => {
    const root = document.documentElement;
    const update = () => setLight(getComputedStyle(root).colorScheme === "light");
    update();
    const observer = new MutationObserver(update);
    observer.observe(root, { attributes: true });
    return () => observer.disconnect();
  }, []);
  return (
    <FileDockContext.Provider value={context}>
      <DockviewReact className="file-dock" theme={light ? themeLight : themeDark}
        components={filePanelComponents} onReady={onReady}
        watermarkComponent={FileDockEmpty} defaultTabComponent={FileDockTab}
        getTabContextMenuItems={() => []} disableFloatingGroups />
    </FileDockContext.Provider>
  );
}

export function focusFile(dock: DockviewApi, path: string, view: FileView = "editor") {
  const id = `${view}:${path}`;
  const existing = dock.getPanel(id);
  if (existing) {
    existing.api.setActive();
  } else {
    dock.addPanel({ id, component: "file", params: { path, view }, title: fileName(path) });
  }
}

export function splitFile(dock: DockviewApi, path: string, view: FileView = "editor") {
  const active = dock.activePanel;
  if (!active) return;
  const id = view === "reading" ? `reading:${path}` : `split:${path}`;
  const existing = dock.getPanel(id);
  if (existing) {
    existing.api.setActive();
    return;
  }
  dock.addPanel({ id, component: "file", params: { path, view }, title: fileName(path), position: { referencePanel: active.id, direction: "right" } });
}

export function keepOnlyCleanClose(dock: DockviewApi, path: string, api: FilePanelApi) {
  return dock.panels.filter((panel) => {
    const params = panel.params as FilePanelParams | undefined;
    return params?.path === path && params.view === "editor" && panel.id !== api.id;
  }).length > 0 || (dock.getPanel(api.id)?.params as FilePanelParams | undefined)?.view === "reading";
}

export function useFileDock({ checkoutId, workspace, simulated, initialPath }: {
  checkoutId: string;
  workspace: "ide" | "knowledge";
  simulated: boolean;
  initialPath?: string;
}) {
  const [api, setApi] = useState<DockviewApi>();
  const [ready, setReady] = useState(false);
  const [path, setPath] = useState<string>();
  const [error, setError] = useState("");
  const initialPathRef = useRef(initialPath);
  const onReady = useCallback(({ api: view }: DockviewReadyEvent) => setApi(view), []);

  useEffect(() => {
    if (!api) return;
    let active = true;
    let timer: number | undefined;
    let pending: string | undefined;
    let loaded = false;
    let writes = Promise.resolve();
    const scope = { workspace, checkoutId };
    const serialize = () => JSON.stringify(api.toJSON());
    const flush = () => {
      const layout = pending;
      pending = undefined;
      if (layout === undefined) return;
      writes = writes.catch(() => undefined).then(() =>
        invoke<void>("save_workbench_layout", { ...scope, layout }),
      );
      void writes.catch((failure: unknown) => { if (active) setError(String(failure)); });
    };
    const save = () => {
      if (simulated || !loaded) return;
      window.clearTimeout(timer);
      pending = serialize();
      timer = window.setTimeout(flush, 80);
    };
    const updatePath = () => setPath((api.activePanel?.params as FilePanelParams | undefined)?.path);
    const activeChange = api.onDidActivePanelChange(() => { updatePath(); save(); });
    const layoutChange = api.onDidLayoutChange(save);
    window.addEventListener("pagehide", flush);

    const restore = async () => {
      try {
        const stored = simulated ? undefined : await invoke<string | null>("workbench_layout", scope);
        if (!active) return;
        if (stored) api.fromJSON(JSON.parse(stored) as SerializedDockview);
      } catch (failure) {
        if (active) setError(String(failure));
      } finally {
        if (active) {
          loaded = true;
          if (initialPathRef.current) focusFile(api, initialPathRef.current);
          updatePath();
          setReady(true);
        }
      }
    };
    void restore();
    return () => {
      active = false;
      window.clearTimeout(timer);
      window.removeEventListener("pagehide", flush);
      flush();
      layoutChange.dispose();
      activeChange.dispose();
    };
  }, [api, checkoutId, simulated, workspace]);

  const open = useCallback((file: string) => { if (api && ready) focusFile(api, file); }, [api, ready]);
  const split = useCallback((file: string, view?: FileView) => { if (api && ready) splitFile(api, file, view); }, [api, ready]);

  return { api, error, onReady, open, path, ready, split };
}

interface FolderNodeData {
  folders: Map<string, FolderNodeData>;
  files: string[];
}

function makeTree(paths: readonly string[]): FolderNodeData {
  const root: FolderNodeData = { folders: new Map(), files: [] };
  for (const path of [...paths].sort((left, right) => left.localeCompare(right))) {
    const parts = path.split("/");
    let node = root;
    for (const name of parts.slice(0, -1)) {
      const folder = node.folders.get(name) ?? { folders: new Map(), files: [] };
      node.folders.set(name, folder);
      node = folder;
    }
    node.files.push(path);
  }
  return root;
}

function FolderBranch({ tree, active, onOpen }: { tree: FolderNodeData; active?: string; onOpen: (path: string) => void }) {
  return (
    <ul className="file-tree">
      {[...tree.folders].map(([name, folder]) => (
        <li key={name}>
          <details open>
            <summary><FolderOpen size={15} />{name}</summary>
            <FolderBranch tree={folder} active={active} onOpen={onOpen} />
          </details>
        </li>
      ))}
      {tree.files.map((path) => {
        const Icon = path.endsWith(".md") ? FileText : FileCode2;
        return (
          <li key={path}>
            <button type="button" className="file-tree-row" aria-current={active === path ? "page" : undefined}
              title={path} onClick={() => onOpen(path)}>
              <Icon size={15} /> <span>{fileName(path)}</span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

export function FileTree({ paths, active, onOpen }: { paths: readonly string[]; active?: string; onOpen: (path: string) => void }) {
  return <FolderBranch tree={makeTree(paths)} active={active} onOpen={onOpen} />;
}

export function FileCaption({ path, revision }: { path: string; revision: number | string }) {
  const label = typeof revision === "number" ? `r${revision}` : revision;
  return <div className="ide-file-caption">
    <span title={path}>{path.replaceAll("/", " / ")}</span>
    <span title={`Revision ${label}`}>{label}</span>
  </div>;
}

export function FileUnavailable({ title, path, detail, onSimulate }: {
  title: string; path?: string; detail?: string; onSimulate?: () => void;
}) {
  return (
    <section className="file-unavailable">
      <span className="work-empty-icon"><FileText size={23} aria-hidden="true" /></span>
      <h2>{title}</h2>
      {path && <p><code>{path}</code></p>}
      {detail && <p>{detail}</p>}
      {onSimulate && <button type="button" className="fw-btn fw-primary" onClick={onSimulate}>Simulate</button>}
    </section>
  );
}

export function FileSidebar({ name, children }: { name: string; children: ReactNode }) {
  return <aside className="file-workspace-sidebar" aria-label={name}>{children}</aside>;
}

export function FileDialog({ title, description, onClose, children }: {
  title: string;
  description: string;
  onClose: () => void;
  children: ReactNode;
}) {
  const activator = useRef(document.activeElement);
  return (
    <Dialog.Root open onOpenChange={(open) => { if (!open) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="file-dialog-overlay" />
        <Dialog.Content className="file-dialog" onCloseAutoFocus={(event) => {
          const element = activator.current;
          if (element instanceof HTMLElement && element.isConnected) {
            event.preventDefault();
            element.focus();
          }
        }}>
          <header>
            <Dialog.Title>{title}</Dialog.Title>
            <Dialog.Close asChild><button type="button" className="fw-icon" aria-label="Close dialog"><X size={17} /></button></Dialog.Close>
          </header>
          <Dialog.Description>{description}</Dialog.Description>
          {children}
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
}

export function SidebarToggle({ open, setOpen, label }: { open: boolean; setOpen: (open: boolean) => void; label: string }) {
  return (
    <button type="button" className="fw-icon" aria-label={label} aria-expanded={open}
      onClick={() => setOpen(!open)} title={label}><PanelLeftClose size={16} /></button>
  );
}

export function useFileSidebar() {
  const [open, setOpen] = useState(() => window.innerWidth >= 960);
  useEffect(() => {
    const viewport = window.matchMedia?.("(min-width: 960px)");
    if (!viewport) return;
    const update = () => setOpen(viewport.matches);
    viewport.addEventListener("change", update);
    return () => viewport.removeEventListener("change", update);
  }, []);
  return [open, setOpen] as const;
}
