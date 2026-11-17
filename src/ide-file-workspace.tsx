import { invoke } from "@tauri-apps/api/core";
import {
  DockviewReact, themeDark, themeLight, type DockviewApi, type DockviewReadyEvent,
  type IDockviewPanelHeaderProps, type IDockviewPanelProps, type SerializedDockview,
} from "dockview-react";
import { FileCode2, FileText, Folder, FolderOpen, FolderTree, PanelLeftClose, Pin, X } from "lucide-react";
import { createContext, useCallback, useContext, useEffect, useRef, useState, type MouseEvent, type ReactNode } from "react";
import { Dialog } from "radix-ui";
import { ContextActions, type MenuGroups } from "@/components/ui/context-actions";
import { fileName } from "@/ide-knowledge-data";
import { ResizablePanel } from "@/panel-resize";
import "dockview/dist/styles/dockview.css";
import "./ide-knowledge.css";

export type FileView = "editor" | "reading";

interface FilePanelParams {
  path: string;
  view: FileView;
  pinned?: boolean;
}

export type FilePanelApi = IDockviewPanelProps["api"];

export interface FileDockContextValue {
  renderFile: (path: string, view: FileView) => ReactNode;
  empty: ReactNode;
  dirty: (path: string) => boolean;
  requestClose: (path: string, api: FilePanelApi) => void;
  tabActions: (path: string, view: FileView, api: FilePanelApi) => MenuGroups;
}

const FileDockContext = createContext<FileDockContextValue | null>(null);

function FileDockPanel({ params }: IDockviewPanelProps<FilePanelParams>) {
  return useContext(FileDockContext)?.renderFile(params.path, params.view) ?? null;
}

function FileDockTab({ params, api }: IDockviewPanelHeaderProps<FilePanelParams>) {
  const context = useContext(FileDockContext);
  const title = fileName(params.path) + (params.view === "reading" ? " · Reading" : "");
  const pinned = params.pinned ?? false;
  const cleanTabs = api.group.panels.filter((panel) => {
    const item = panel.params as FilePanelParams | undefined;
    return panel.id !== api.id && item && !item.pinned && !context?.dirty(item.path);
  });
  return (
    <ContextActions label={title} className="file-tab-label" title={params.path} groups={[
      ...(context?.tabActions(params.path, params.view, api) ?? []),
      [
        { label: pinned ? "Unpin tab" : "Pin tab", run: () => api.updateParameters({ ...params, pinned: !pinned }) },
        { label: "Close other saved tabs in group", disabled: !cleanTabs.length, run: () => {
          for (const panel of cleanTabs) {
            const item = panel.params as FilePanelParams;
            if (!item.pinned && !context?.dirty(item.path)) context?.requestClose(item.path, panel.api);
          }
        } },
      ],
    ]} stopPointerDown>
      {pinned && <Pin size={12} aria-label="Pinned" />}
      <span>{title}</span>
      {params.view === "editor" && context?.dirty(params.path) && <i aria-label="Unsaved changes" />}
      <button type="button" aria-label={`Close ${title}`} onPointerDown={(event) => event.stopPropagation()}
        onClick={(event) => { event.stopPropagation(); context?.requestClose(params.path, api); }}><X size={13} /></button>
    </ContextActions>
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
  const existing = dock.panels.find((panel) => panel.params?.path === path && panel.params?.view === view);
  if (existing) {
    existing.api.setActive();
  } else {
    dock.addPanel({ id: unusedPanelId(dock, id), component: "file", params: { path, view }, title: fileName(path) });
  }
}

function unusedPanelId(dock: DockviewApi, preferred: string) {
  return dock.getPanel(preferred) ? `${preferred}:${crypto.randomUUID()}` : preferred;
}

export function splitFile(dock: DockviewApi, path: string, view: FileView = "editor", direction: "right" | "below" = "right", reference?: string) {
  const active = reference ? dock.getPanel(reference) : dock.activePanel;
  if (!active) { focusFile(dock, path, view); return; }
  const id = view === "reading" ? `reading:${path}` : direction === "below" ? `split-below:${path}` : `split:${path}`;
  const existing = dock.getPanel(id);
  if (existing?.params?.path === path && existing.params?.view === view) {
    existing.api.setActive();
    return;
  }
  dock.addPanel({ id: unusedPanelId(dock, id), component: "file", params: { path, view }, title: fileName(path),
    position: { referencePanel: active.id, direction } });
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
  const [file, setFile] = useState<FilePanelParams>();
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
    const updateFile = () => setFile(api.activePanel?.params as FilePanelParams | undefined);
    const activeChange = api.onDidActivePanelChange(() => { updateFile(); save(); });
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
          updateFile();
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
  const split = useCallback((file: string, view?: FileView, direction?: "right" | "below", reference?: string) => {
    if (api && ready) splitFile(api, file, view, direction, reference);
  }, [api, ready]);

  const reconcilePaths = (destination: (path: string) => string | null | undefined) => {
    const reopened = new Set<string>();
    if (!api) return reopened;
    for (const panel of [...api.panels]) {
      const params = panel.params as FilePanelParams | undefined;
      if (!params) continue;
      const path = destination(params.path);
      if (path === null) panel.api.close();
      else if (path !== undefined && path !== params.path) {
        panel.api.updateParameters({ ...params, path });
        panel.api.setTitle(fileName(path));
        reopened.add(path);
      }
    }
    setFile(api.activePanel?.params as FilePanelParams | undefined);
    return reopened;
  };

  return { api, error, onReady, open, path: file?.path, view: file?.view, ready, split, reconcilePaths };
}

interface FolderNodeData {
  folders: Map<string, FolderNodeData>;
  files: string[];
}

function makeTree(paths: readonly string[], folders: readonly string[]): FolderNodeData {
  const root: FolderNodeData = { folders: new Map(), files: [] };
  const folderAt = (parts: string[]) => {
    let node = root;
    for (const name of parts) {
      const folder = node.folders.get(name) ?? { folders: new Map(), files: [] };
      node.folders.set(name, folder);
      node = folder;
    }
    return node;
  };
  for (const folder of [...folders].sort((left, right) => left.localeCompare(right))) folderAt(folder.split("/"));
  for (const path of [...paths].sort((left, right) => left.localeCompare(right))) {
    folderAt(path.split("/").slice(0, -1)).files.push(path);
  }
  return root;
}

export type FileTreeTarget = { kind: "file" | "folder" | "root"; path: string };

interface TreeRows {
  active?: string;
  choose: (target: FileTreeTarget, event: MouseEvent<HTMLButtonElement>) => void;
  actions: (target: FileTreeTarget, selection?: readonly FileTreeTarget[]) => MenuGroups;
  expanded: ReadonlySet<string>;
  selected: ReadonlySet<string>;
  selection: (path: string) => readonly FileTreeTarget[] | undefined;
  setSubtree: (path: string, collapse: boolean) => void;
}

function folderPaths(tree: FolderNodeData, parent = ""): string[] {
  return [...tree.folders].flatMap(([name, folder]) => {
    const path = parent ? `${parent}/${name}` : name;
    return [path, ...folderPaths(folder, path)];
  });
}

function FolderBranch({ tree, parent, rows }: { tree: FolderNodeData; parent: string; rows: TreeRows }) {
  return (
    <ul className="file-tree">
      {[...tree.folders].map(([name, folder]) => {
        const path = parent ? `${parent}/${name}` : name;
        const expanded = rows.expanded.has(path);
        const Icon = expanded ? FolderOpen : Folder;
        const target: FileTreeTarget = { kind: "folder", path };
        const selected = rows.selection(path);
        return <li key={name}>
          <ContextActions label={selected ? `${selected.length} selected items` : path} groups={[...rows.actions(target, selected), ...(!selected ? [[
            { label: "Expand folder tree", run: () => rows.setSubtree(path, false) },
            { label: "Collapse folder tree", run: () => rows.setSubtree(path, true) },
          ]] : [])]}>
            <button type="button" className="file-tree-row" aria-expanded={expanded} aria-pressed={rows.selected.has(path) || undefined} title={path}
              onClick={(event) => rows.choose(target, event)}>
              <Icon size={15} /><span>{name}</span>
            </button>
          </ContextActions>
          {expanded && <FolderBranch tree={folder} parent={path} rows={rows} />}
        </li>;
      })}
      {tree.files.map((path) => {
        const Icon = /\.(?:md|markdown)$/i.test(path) ? FileText : FileCode2;
        const target: FileTreeTarget = { kind: "file", path };
        const selected = rows.selection(path);
        return (
          <li key={path}>
            <ContextActions label={selected ? `${selected.length} selected items` : path} groups={rows.actions(target, selected)}>
              <button type="button" className="file-tree-row" aria-current={rows.active === path ? "page" : undefined}
                title={path} aria-pressed={rows.selected.has(path) || undefined} onClick={(event) => rows.choose(target, event)}>
                <Icon size={15} /> <span>{fileName(path)}</span>
              </button>
            </ContextActions>
          </li>
        );
      })}
    </ul>
  );
}

export function FileTree({ paths, folders = [], active, onOpen, actions, reveal, checkoutId, workspace, simulated }: {
  paths: readonly string[];
  folders?: readonly string[];
  active?: string;
  onOpen: (path: string) => void;
  actions: TreeRows["actions"];
  reveal?: { path: string };
  checkoutId: string;
  workspace: "ide" | "knowledge";
  simulated: boolean;
}) {
  const tree = makeTree(paths, folders);
  const storageKey = `omniagent:file-tree:${workspace}:${simulated ? "example" : encodeURIComponent(checkoutId)}`;
  return <PersistentFolderTree key={storageKey} {...{ tree, active, onOpen, actions, reveal, storageKey }} />;
}

function PersistentFolderTree({ tree, active, onOpen, actions, reveal, storageKey }: {
  tree: FolderNodeData; reveal?: { path: string }; storageKey: string; onOpen: (path: string) => void;
} & Pick<TreeRows, "active" | "actions">) {
  const [expanded, setExpanded] = useState(() => {
    try {
      const saved: unknown = JSON.parse(localStorage.getItem(storageKey) ?? "[]");
      if (Array.isArray(saved)) return new Set<string>(saved.filter((path) => typeof path === "string"));
    } catch { /* Use the default when storage is unavailable. */ }
    return new Set<string>();
  });
  const container = useRef<HTMLDivElement>(null);
  const [selected, setSelected] = useState(new Set<string>());
  const selectionAnchor = useRef<string>(undefined);
  function visibleItems(node: FolderNodeData, parent = ""): FileTreeTarget[] {
    return [...[...node.folders].flatMap(([name, folder]): FileTreeTarget[] => {
      const path = parent ? `${parent}/${name}` : name;
      return [{ kind: "folder", path }, ...(expanded.has(path) ? visibleItems(folder, path) : [])];
    }), ...node.files.map((path): FileTreeTarget => ({ kind: "file", path }))];
  }
  const selection = selected.size > 1 ? visibleItems(tree).filter(({ path }) => selected.has(path)) : [];
  function choose(target: FileTreeTarget, event: MouseEvent<HTMLButtonElement>) {
    const { path, kind } = target;
    if (event.shiftKey && selectionAnchor.current) {
      const visible = visibleItems(tree);
      const first = visible.findIndex((item) => item.path === selectionAnchor.current);
      const last = visible.findIndex((item) => item.path === path);
      if (first >= 0 && last >= 0) {
        const range = visible.slice(Math.min(first, last), Math.max(first, last) + 1).map((item) => item.path);
        setSelected(new Set([...((event.ctrlKey || event.metaKey) ? selected : []), ...range]));
        return;
      }
    }
    if (event.ctrlKey || event.metaKey || event.shiftKey) {
      const next = new Set(selected.size ? selected : selectionAnchor.current ? [selectionAnchor.current] : []);
      if (!next.delete(path)) next.add(path);
      setSelected(next);
    } else {
      setSelected(new Set());
      if (kind === "file") onOpen(path);
      else if (kind === "folder") setExpanded((previous) => {
        const next = new Set(previous);
        if (!next.delete(path)) next.add(path);
        return next;
      });
    }
    selectionAnchor.current = path;
  }
  useEffect(() => {
    try { localStorage.setItem(storageKey, JSON.stringify([...expanded])); }
    catch { /* Keep the in-memory state when storage is unavailable. */ }
  }, [expanded, storageKey]);
  useEffect(() => {
    if (!reveal) return;
    const path = reveal.path;
    const parents = path.split("/").slice(0, -1).map((_, index, parts) => parts.slice(0, index + 1).join("/"));
    setExpanded((previous) => new Set([...previous, ...parents]));
    const frame = requestAnimationFrame(() => {
      const button = [...(container.current?.querySelectorAll<HTMLButtonElement>(".file-tree-row") ?? [])]
        .find((item) => item.title === path);
      button?.focus();
      button?.scrollIntoView?.({ block: "nearest" });
    });
    return () => cancelAnimationFrame(frame);
  }, [reveal]);
  function setSubtree(path: string, collapse: boolean) {
    const selected = new Set(folderPaths(tree).filter((folder) => !path || folder === path || folder.startsWith(`${path}/`)));
    setExpanded((previous) => collapse ? new Set([...previous].filter((folder) => !selected.has(folder)))
      : new Set([...previous, ...selected]));
  }
  return <div ref={container} className="file-explorer-tree">
    <ContextActions label="Checkout root" className="file-tree-root" groups={[...actions({ kind: "root", path: "" }), [
      { label: "Expand all folders", run: () => setSubtree("", false) },
      { label: "Collapse all folders", run: () => setSubtree("", true) },
    ]]}>
      <button type="button" className="file-tree-row" onClick={(event) => event.currentTarget.focus()}>
        <FolderTree size={15} /><span>Checkout root</span>
      </button>
    </ContextActions>
    {selection.length > 1 && <p className="file-selection-count" role="status">{selection.length} selected</p>}
    <FolderBranch tree={tree} parent="" rows={{ active, actions, expanded, setSubtree, selected, choose,
      selection: (path) => selection.length > 1 && selected.has(path) ? selection : undefined }} />
  </div>;
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
  return <ResizablePanel as="aside" className="file-workspace-sidebar" aria-label={name} edge="right" resizeKey={name} resizeLabel={name}>{children}</ResizablePanel>;
}

export function FileDialog({ title, description, onClose, children, className = "" }: {
  title: string;
  description: string;
  onClose: () => void;
  children: ReactNode;
  className?: string;
}) {
  const activator = useRef(document.activeElement);
  return (
    <Dialog.Root open onOpenChange={(open) => { if (!open) onClose(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="file-dialog-overlay" />
        <Dialog.Content className={`file-dialog ${className}`} onCloseAutoFocus={(event) => {
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
