import { invoke } from "@tauri-apps/api/core";
import {
  DockviewReact, themeDark, themeLight, type DockviewApi, type DockviewReadyEvent,
  type IDockviewHeaderActionsProps, type IDockviewPanelHeaderProps, type IDockviewPanelProps, type IWatermarkPanelProps,
  type SerializedDockview,
} from "dockview-react";
import { FileCode2, FileText, Folder, FolderOpen, FolderTree, PanelLeftClose, Pin, X } from "lucide-react";
import { createContext, useCallback, useContext, useEffect, useRef, useState, type MouseEvent, type ReactNode } from "react";
import { Dialog } from "radix-ui";
import { ContextActions, PositionedContextActions, type MenuGroups } from "@/components/ui/context-actions";
import { fileName } from "@/ide-knowledge-data";
import { setCheckoutFavorites, useCheckoutFavorites, type CheckoutFavorite } from "@/checkout-favorites";
import { dockviewLayoutInMain, editorPopoutPath, FileEditorDocument, mirrorEditorAppearance } from "@/file-editor-popout";
import { makeTree, readTreePreferences, saveTreePreference, showTreePath, treeParents, useIgnoredTree, useWindowsHiddenTree,
  type FolderNodeData, type TreeReveal } from "@/checkout-file-tree";
import { ResizablePanel } from "@/panel-resize";
import "dockview/dist/styles/dockview.css";
import "./ide-knowledge.css";
import "./checkout-file-tree.css";
import "./file-dock-actions.css";

export type FileView = "editor" | "reading";

interface FilePanelParams {
  path: string;
  view: FileView;
  pinned?: boolean;
}

export type FilePanelApi = IDockviewPanelProps["api"];
type DockGroup = DockviewApi["groups"][number];
type DockTab = DockviewApi["panels"][number];
type CloseTab = (path: string, api: FilePanelApi) => boolean;
type ReopenFile = (path: string) => string | undefined | Promise<string | undefined>;

type FileDockCommands = {
  dirty: (path: string) => boolean;
  requestClose: CloseTab;
  loadClosedFile: ReopenFile;
};

type ClosedTab = { sequence: number; id: string; params: FilePanelParams; groupId: string; index: number };
type CloseBatch = { ids: string[]; savedOnly: boolean };

export interface FileDockContextValue {
  renderFile: (path: string, view: FileView, panelId: string) => ReactNode;
  empty: ReactNode;
  dirty: (path: string) => boolean;
  requestClose: CloseTab;
  tabActions: (path: string, view: FileView, api: FilePanelApi) => MenuGroups;
  layoutActions: (tab: FilePanelApi) => MenuGroups;
  groupActions: (groupId?: string) => MenuGroups;
}

const FileDockContext = createContext<FileDockContextValue | null>(null);

function panelDocument(api?: FilePanelApi) {
  const owner = api?.getWindow();
  return owner && !owner.closed ? owner.document ?? document : document;
}

function usePanelDocument(api?: FilePanelApi) {
  const [owner, setOwner] = useState(() => panelDocument(api));
  useEffect(() => {
    const update = () => setOwner(panelDocument(api));
    update();
    const location = api?.onDidLocationChange(update);
    return () => location?.dispose();
  }, [api]);
  return owner;
}

function FileDockPanel({ params, api }: IDockviewPanelProps<FilePanelParams>) {
  const context = useContext(FileDockContext);
  const ownerDocument = usePanelDocument(api);
  return <FileEditorDocument.Provider value={ownerDocument}>
    {context?.renderFile(params.path, params.view, api.id)}
  </FileEditorDocument.Provider>;
}

function FileDockTab({ params, api }: IDockviewPanelHeaderProps<FilePanelParams>) {
  const context = useContext(FileDockContext);
  const title = fileName(params.path) + (params.view === "reading" ? " · Reading" : "");
  const pinned = params.pinned ?? false;
  return (
    <ContextActions label={title} className="file-tab-label" title={params.path} groups={[
      ...(context?.tabActions(params.path, params.view, api) ?? []),
      [{ label: pinned ? "Unpin tab" : "Pin tab", run: () => api.updateParameters({ ...params, pinned: !pinned }) }],
      ...(context?.layoutActions(api) ?? []),
    ]} stopPointerDown>
      {pinned && <Pin size={12} aria-label="Pinned" />}
      <span>{title}</span>
      {params.view === "editor" && context?.dirty(params.path) && <i aria-label="Unsaved changes" />}
      <button type="button" aria-label={`Close ${title}`} onPointerDown={(event) => event.stopPropagation()}
        onClick={(event) => { event.stopPropagation(); context?.requestClose(params.path, api); }}><X size={13} /></button>
    </ContextActions>
  );
}

function FileDockEmpty({ group }: IWatermarkPanelProps) {
  const context = useContext(FileDockContext);
  return <ContextActions label="Editor area" className="file-dock-empty-menu"
    groups={context?.groupActions(group?.id) ?? []}>{context?.empty}</ContextActions>;
}

function FileGroupControls({ group }: IDockviewHeaderActionsProps) {
  const context = useContext(FileDockContext);
  return <ContextActions label="Editor group" className="file-dock-group-menu" groups={context?.groupActions(group.id) ?? []}
    stopPointerDown><></></ContextActions>;
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
        watermarkComponent={FileDockEmpty} defaultTabComponent={FileDockTab} rightHeaderActionsComponent={FileGroupControls}
        getTabContextMenuItems={() => []} disableFloatingGroups popoutUrl={editorPopoutPath} />
    </FileDockContext.Provider>
  );
}

export function focusFile(dock: DockviewApi, path: string, view: FileView = "editor", home?: DockGroup) {
  const id = `${view}:${path}`;
  const existing = dock.panels.find((panel) => panel.params?.path === path && panel.params?.view === view);
  if (existing) {
    existing.api.setActive();
    if (existing.group.api.location.type === "popout") existing.api.getWindow().focus();
    return existing;
  }
  return dock.addPanel({ id: unusedPanelId(dock, id), component: "file", params: { path, view }, title: fileName(path),
    position: home ? { referenceGroup: home } : undefined });
}

function unusedPanelId(dock: DockviewApi, preferred: string) {
  return dock.getPanel(preferred) ? `${preferred}:${crypto.randomUUID()}` : preferred;
}

export function splitFile(dock: DockviewApi, path: string, view: FileView = "editor", direction: "right" | "below" = "right", reference?: string) {
  const active = reference ? dock.getPanel(reference) : dock.activePanel;
  if (!active) {
    if (!reference) focusFile(dock, path, view);
    return;
  }
  const id = view === "reading" ? `reading:${path}` : direction === "below" ? `split-below:${path}` : `split:${path}`;
  const existing = dock.getPanel(id);
  if (!reference && existing?.params?.path === path && existing.params?.view === view) {
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
  const [closed, setClosed] = useState<ClosedTab[]>([]);
  const [reopening, setReopening] = useState(false);
  const closeSequence = useRef(0);
  const pendingClose = useRef<CloseBatch>(null);
  const reopenPending = useRef(false);
  const detaching = useRef(false);
  const liveDock = useRef<DockviewApi | null>(null);
  const initialPathRef = useRef(initialPath);
  const onReady = useCallback(({ api: view }: DockviewReadyEvent) => setApi(view), []);

  useEffect(() => {
    if (!api) return;
    liveDock.current = api;
    let active = true;
    let timer: number | undefined;
    let pending: string | undefined;
    let loaded = false;
    let writes = Promise.resolve();
    const scope = { workspace, checkoutId };
    const serialize = () => JSON.stringify(dockviewLayoutInMain(api.toJSON()));
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
    const cleanupPopouts = new Map<Window, () => void>();
    const addPopout = api.onDidAddPopoutGroup(({ group, window: popup }) => {
      popup.document.querySelector(".dv-dockview")?.classList.add("file-dock");
      const stopAppearance = mirrorEditorAppearance(popup);
      const title = () => { popup.document.title = (group.activePanel?.title ?? "Editor group") + " · OmniAgent"; };
      title();
      const activePanel = group.api.onDidActivePanelChange(title);
      cleanupPopouts.set(popup, () => { activePanel.dispose(); stopAppearance(); });
      save();
    });
    const closePopout = api.onWillClosePopoutWindow(({ window: popup }) => {
      cleanupPopouts.get(popup)?.();
      cleanupPopouts.delete(popup);
    });
    window.addEventListener("pagehide", flush);

    const restore = async () => {
      try {
        const stored = simulated ? undefined : await invoke<string | null>("workbench_layout", scope);
        if (!active) return;
        if (stored) api.fromJSON(dockviewLayoutInMain(JSON.parse(stored) as SerializedDockview));
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
      liveDock.current = null;
      pendingClose.current = null;
      window.clearTimeout(timer);
      window.removeEventListener("pagehide", flush);
      flush();
      layoutChange.dispose();
      activeChange.dispose();
      addPopout.dispose();
      closePopout.dispose();
      for (const cleanup of cleanupPopouts.values()) cleanup();
    };
  }, [api, checkoutId, simulated, workspace]);

  const open = useCallback((file: string) => { if (api && ready) focusFile(api, file); }, [api, ready]);
  const detach = useCallback(async (target: DockTab | DockGroup) => {
    if (!api || !ready || detaching.current) return;
    detaching.current = true;
    setError("");
    try {
      const open = await api.addPopoutGroup(target);
      if (!open && liveDock.current === api) {
        setError("Could not open an editor window.");
      }
    } catch (failure) {
      if (liveDock.current === api) setError(String(failure));
    } finally {
      detaching.current = false;
    }
  }, [api, ready]);
  const openInWindow = useCallback((path: string, view: FileView = "editor") => {
    if (!api || !ready) return;
    const detached = api.panels.find((panel) => panel.params?.path === path && panel.params?.view === view
      && panel.group.api.location.type === "popout");
    if (detached) {
      detached.api.setActive();
      detached.api.getWindow().focus();
      return;
    }
    const home = api.groups.find((group) => group.api.location.type === "grid");
    const panel = focusFile(api, path, view, home);
    void detach(panel);
  }, [api, detach, ready]);
  const split = useCallback((file: string, view?: FileView, direction?: "right" | "below", reference?: string) => {
    if (api && ready) splitFile(api, file, view, direction, reference);
  }, [api, ready]);

  function closeTab(tab: FilePanelApi) {
    const panel = api?.getPanel(tab.id);
    const params = panel?.params as FilePanelParams | undefined;
    if (!panel || !params) return false;
    const entry = { sequence: ++closeSequence.current, id: panel.id, params: { ...params },
      groupId: tab.group.id, index: tab.group.panels.findIndex(({ id }) => id === panel.id) };
    tab.close();
    setClosed((previous) => [entry, ...previous].slice(0, 20));
    return true;
  }

  const cancelCloseBatch = useCallback(() => { pendingClose.current = null; }, []);

  function resumeCloseBatch({ requestClose, dirty }: Pick<FileDockCommands, "requestClose" | "dirty">) {
    const batch = pendingClose.current;
    if (!api || !batch) return;
    for (let id = batch.ids.shift(); id !== undefined; id = batch.ids.shift()) {
      const panel = api.getPanel(id);
      const params = panel?.params as FilePanelParams | undefined;
      if (!panel || !params || params.pinned || (batch.savedOnly && dirty(params.path))) continue;
      if (!requestClose(params.path, panel.api)) return;
    }
    pendingClose.current = null;
  }

  function closeTabs(tabs: readonly DockTab[], commands: FileDockCommands, savedOnly = false) {
    pendingClose.current = { ids: tabs.map(({ id }) => id), savedOnly };
    resumeCloseBatch(commands);
  }

  async function reopen(tab: ClosedTab, load: ReopenFile, groupId?: string) {
    if (!api || !ready || reopenPending.current) return;
    const destination = groupId ?? tab.groupId;
    reopenPending.current = true;
    setReopening(true);
    setError("");
    try {
      const path = await load(tab.params.path);
      if (liveDock.current !== api) return;
      if (!path) { setError(`Could not reopen ${tab.params.path}.`); return; }
      const group = api.groups.find(({ id }) => id === destination) ?? api.activeGroup;
      const position = group ? { referenceGroup: group, index: group.id === tab.groupId ? Math.min(tab.index, group.panels.length) : undefined } : undefined;
      api.addPanel({ id: unusedPanelId(api, tab.id), component: "file", params: { ...tab.params, path }, title: fileName(path), position });
      setClosed((previous) => previous.filter((entry) => entry.sequence !== tab.sequence));
    } catch (failure) {
      if (liveDock.current === api) setError(String(failure));
    } finally {
      reopenPending.current = false;
      if (liveDock.current === api) setReopening(false);
    }
  }

  function groupName(group: DockGroup) {
    const index = api?.groups.indexOf(group) ?? -1;
    const path = (group.activePanel?.params as FilePanelParams | undefined)?.path;
    return `Group ${index + 1}${path ? ` · ${fileName(path)}` : ""}`;
  }

  function reopenActions(commands: FileDockCommands, groupId?: string) {
    const latest = closed[0];
    if (!latest) return [];
    const unique = new Map<string, ClosedTab>();
    for (const item of closed) {
      const label = item.params.path + (item.params.view === "reading" ? " · Reading" : "");
      if (!unique.has(label)) unique.set(label, item);
    }
    return [{ label: "Reopen closed tab", disabled: reopening, run: () => void reopen(latest, commands.loadClosedFile, groupId) },
      ...(unique.size > 1 ? [{ label: "Recently closed tabs", disabled: reopening, items: [...unique].map(([label, item]) => ({
        label,
        run: () => void reopen(item, commands.loadClosedFile, groupId),
      })) }] : [])];
  }

  function groupActions(groupId: string | undefined, commands: FileDockCommands): MenuGroups {
    const group = api?.groups.find(({ id }) => id === groupId);
    if (!group || !api) return [reopenActions(commands)];
    const unpinned = group.panels.filter((panel) => !panel.params?.pinned);
    const saved = unpinned.filter((panel) => !commands.dirty(String(panel.params?.path)));
    const others = api.groups.filter((other) => other.id !== group.id);
    const focused = group.activePanel;
    const current = focused?.params as FilePanelParams | undefined;
    return [[
      ...(focused && current ? [
        { label: "Split active tab right", run: () => split(current.path, current.view, "right", focused.id) },
        { label: "Split active tab down", run: () => split(current.path, current.view, "below", focused.id) },
      ] : []),
      ...(group.api.location.type === "popout" ? [
        { label: "Return editor window to main window", run: () => group.api.getWindow().close() },
      ] : [
        { label: "Move group to new window", disabled: !group.panels.length, run: () => void detach(group) },
      ]),
      ...(others.length ? [{ label: "Merge this group into", items: others.map((other) => ({
        label: groupName(other), run: () => mergeGroups(group, other),
      })) }, { label: "Merge all groups here", run: () => {
        for (const other of others) mergeGroups(other, group);
      } }] : []),
    ], [
      { label: "Close saved tabs in group", disabled: !saved.length, run: () => closeTabs(saved, commands, true) },
      { label: unpinned.length === group.panels.length ? "Close editor group" : "Close unpinned tabs in group",
        disabled: !unpinned.length, run: () => closeTabs(unpinned, commands) },
    ], reopenActions(commands, group.id)];
  }

  function mergeGroups(source: DockGroup, target: DockGroup) {
    if (api?.groups.includes(source) && api.groups.includes(target)) {
      source.api.moveTo({ group: target, position: "center" });
    }
  }

  function layoutActions(tab: FilePanelApi, commands: FileDockCommands): MenuGroups {
    const { group } = tab;
    const index = group.panels.findIndex(({ id }) => id === tab.id);
    const otherTabs = group.panels.filter((panel) => panel.id !== tab.id && !panel.params?.pinned);
    const right = index === -1 ? [] : group.panels.slice(index + 1).filter((panel) => !panel.params?.pinned);
    const saved = otherTabs.filter((panel) => !commands.dirty(String(panel.params?.path)));
    const others = api?.groups.filter((other) => other.id !== group.id) ?? [];
    const main = others.find((other) => other.api.location.type === "grid" && other.panels.length);
    return [[
      { label: "Move tab", items: [
        { label: "New group to the right", disabled: group.panels.length < 2, run: () => tab.moveTo({ group, position: "right" }) },
        { label: "New group below", disabled: group.panels.length < 2, run: () => tab.moveTo({ group, position: "bottom" }) },
        ...others.map((other) => ({ label: groupName(other), run: () => tab.moveTo({ group: other, position: "center" }) })),
      ] },
      ...(group.api.location.type === "popout" ? [
        ...(main && group.panels.length > 1 ? [{ label: "Move tab to main window", run: () => tab.moveTo({ group: main, position: "center" }) }] : []),
        { label: "Return editor window to main window", run: () => group.api.getWindow().close() },
      ] : [{ label: "Move tab to new window", run: () => {
        const selected = api?.getPanel(tab.id);
        if (selected) void detach(selected);
      } }]),
    ], [
      { label: "Close other tabs in group", disabled: !otherTabs.length, run: () => closeTabs(otherTabs, commands) },
      { label: "Close tabs to the right", disabled: !right.length, run: () => closeTabs(right, commands) },
      { label: "Close other saved tabs in group", disabled: !saved.length, run: () => closeTabs(saved, commands, true) },
    ], reopenActions(commands, group.id)];
  }

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
    setClosed((previous) => previous.flatMap((item) => {
      const path = destination(item.params.path);
      if (path === null) return [];
      return path === undefined ? [item] : [{ ...item, params: { ...item.params, path } }];
    }));
    setFile(api.activePanel?.params as FilePanelParams | undefined);
    return reopened;
  };

  return { api, error, onReady, open, openInWindow, path: file?.path, view: file?.view, ready, split, reconcilePaths,
    closeTab, cancelCloseBatch, resumeCloseBatch, groupActions, layoutActions };
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
  ignored: ReadonlySet<string>;
  ignoredLoad: ReadonlyMap<string, { pending: boolean; error: Error | null }>;
  windowsHiddenLoad: ReadonlyMap<string, { pending: boolean; error: Error | null }>;
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
        const load = rows.ignoredLoad.get(path);
        const hidden = rows.windowsHiddenLoad.get(path);
        return <li key={name}>
          <ContextActions label={selected ? `${selected.length} selected items` : path} groups={[...rows.actions(target, selected), ...(!selected ? [[
            { label: "Expand folder tree", run: () => rows.setSubtree(path, false) },
            { label: "Collapse folder tree", run: () => rows.setSubtree(path, true) },
          ]] : [])]}>
            <button type="button" className="file-tree-row" data-tree-item data-git-ignored={rows.ignored.has(path) || undefined}
              aria-expanded={expanded} aria-pressed={rows.selected.has(path) || undefined} title={path}
              onClick={(event) => rows.choose(target, event)}>
              <Icon size={15} /><span>{name}</span>
            </button>
          </ContextActions>
          {expanded && load?.pending && <p className="file-help" role="status">Loading {name}…</p>}
          {expanded && load?.error && <p className="file-help" role="alert">{String(load.error)}</p>}
          {expanded && hidden?.pending && <p className="file-help" role="status">Checking Windows hidden items in {name}…</p>}
          {expanded && hidden?.error && <p className="file-help" role="alert">Could not check Windows hidden items in {name}: {String(hidden.error)}</p>}
          {expanded && !hidden?.pending && <FolderBranch tree={folder} parent={path} rows={rows} />}
        </li>;
      })}
      {tree.files.map((path) => {
        const Icon = /\.(?:md|markdown)$/i.test(path) ? FileText : FileCode2;
        const target: FileTreeTarget = { kind: "file", path };
        const selected = rows.selection(path);
        return (
          <li key={path}>
            <ContextActions label={selected ? `${selected.length} selected items` : path} groups={rows.actions(target, selected)}>
              <button type="button" className="file-tree-row" data-tree-item data-git-ignored={rows.ignored.has(path) || undefined}
                aria-current={rows.active === path ? "page" : undefined}
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

type FileTreeProps = {
  paths: readonly string[];
  folders?: readonly string[];
  active?: string;
  onOpen: (path: string) => void;
  actions: TreeRows["actions"];
  reveal?: { path: string };
  checkoutId: string;
  workspace: "ide" | "knowledge";
  simulated: boolean;
};

export function FileTree(props: FileTreeProps) {
  const { checkoutId, workspace, simulated } = props;
  const storageKey = `omniagent:file-tree:${workspace}:${simulated ? "example" : encodeURIComponent(checkoutId)}`;
  return <PersistentFolderTree key={storageKey} {...props} storageKey={storageKey} />;
}

function PersistentFolderTree({ paths, folders = [], active, onOpen, actions, reveal, storageKey, checkoutId, workspace, simulated }:
  FileTreeProps & { storageKey: string }) {
  const preferencesKey = `${storageKey}:preferences`;
  const [preferences, setPreferences] = useState(() => readTreePreferences(preferencesKey));
  const [expanded, setExpanded] = useState(() => {
    try {
      const saved: unknown = JSON.parse(localStorage.getItem(storageKey) ?? "[]");
      if (Array.isArray(saved)) return new Set<string>(saved.filter((path) => typeof path === "string"));
    } catch { /* Use the default when storage is unavailable. */ }
    return new Set<string>();
  });
  const container = useRef<HTMLDivElement>(null);
  const rootButton = useRef<HTMLButtonElement>(null);
  const [selected, setSelected] = useState(new Set<string>());
  const [manualReveal, setManualReveal] = useState<TreeReveal>();
  const [visibilityOverride, setVisibilityOverride] = useState<TreeReveal>();
  const [blankMenu, setBlankMenu] = useState<{ x: number; y: number }>();
  const selectionAnchor = useRef<string>(undefined);
  const lastAutoReveal = useRef<string>(undefined);
  const favoritesId = simulated ? "example" : checkoutId;
  const favorites = useCheckoutFavorites(favoritesId);
  const favoritePaths = new Set(favorites.map(({ path }) => path.toLowerCase()));
  const automaticReveal = preferences.autoReveal && active ? { path: active } : undefined;
  const revealPath = manualReveal?.path ?? automaticReveal?.path;
  const visibleReveals = [manualReveal ?? visibilityOverride, automaticReveal].filter((reveal) => reveal !== undefined);
  const ignored = useIgnoredTree(checkoutId, !simulated && preferences.ignored, expanded, preferences.dotfiles, visibleReveals);
  const markdown = (path: string) => /\.(?:md|markdown)$/i.test(path);
  const allFiles = [...paths, ...ignored.files.filter((path) => workspace === "ide" || markdown(path))];
  const allFolders = [...folders, ...ignored.folders];
  const windowsHidden = useWindowsHiddenTree({ checkoutId, enabled: !simulated && !preferences.windowsHidden, expanded,
    files: allFiles, folders: allFolders, dotfiles: preferences.dotfiles, reveals: visibleReveals });
  const visible = (path: string) => showTreePath(path, preferences.dotfiles, visibleReveals, windowsHidden.paths);
  const tree = makeTree(allFiles.filter(visible), allFolders.filter(visible), preferences.sort);
  const ignoredPaths = new Set([...ignored.files, ...ignored.folders]);
  function visibleItems(node: FolderNodeData, parent = ""): FileTreeTarget[] {
    return [...[...node.folders].flatMap(([name, folder]): FileTreeTarget[] => {
      const path = parent ? `${parent}/${name}` : name;
      return [{ kind: "folder", path }, ...(expanded.has(path) && !windowsHidden.load.get(path)?.pending ? visibleItems(folder, path) : [])];
    }), ...node.files.map((path): FileTreeTarget => ({ kind: "file", path }))];
  }
  const selection = selected.size > 1 && !windowsHidden.pending ? visibleItems(tree).filter(({ path }) => selected.has(path)) : [];
  function choose(target: FileTreeTarget, event: MouseEvent<HTMLButtonElement>) {
    const { path, kind } = target;
    if (manualReveal) setManualReveal(undefined);
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
    saveTreePreference(storageKey, [...expanded]);
  }, [expanded, storageKey]);
  useEffect(() => {
    saveTreePreference(preferencesKey, preferences);
  }, [preferences, preferencesKey]);

  useEffect(() => {
    if (!reveal) return;
    setManualReveal(reveal);
    setVisibilityOverride(reveal);
  }, [reveal]);
  useEffect(() => {
    if (!revealPath) return;
    const parents = treeParents(revealPath);
    if (manualReveal?.expand) parents.push(revealPath);
    setExpanded((previous) => parents.every((path) => previous.has(path)) ? previous : new Set([...previous, ...parents]));
  }, [manualReveal, revealPath]);
  useEffect(() => {
    if (!preferences.autoReveal || !active) lastAutoReveal.current = undefined;
  }, [active, preferences.autoReveal]);
  useEffect(() => {
    if (!revealPath) return;
    if (!manualReveal && lastAutoReveal.current === revealPath) return;
    const frame = requestAnimationFrame(() => {
      const button = [...(container.current?.querySelectorAll<HTMLButtonElement>("[data-tree-item]") ?? [])]
        .find((item) => item.title === revealPath);
      if (!button) return;
      if (manualReveal) {
        button.focus();
        setManualReveal(undefined);
      } else lastAutoReveal.current = revealPath;
      button.scrollIntoView?.({ block: "nearest" });
    });
    return () => cancelAnimationFrame(frame);
  }, [manualReveal, revealPath, expanded, paths, folders, ignored.files.length, ignored.folders.length, windowsHidden.pending, windowsHidden.load]);

  function setSubtree(path: string, collapse: boolean) {
    setExpanded((previous) => collapse
      ? new Set([...previous].filter((folder) => path && folder !== path && !folder.startsWith(`${path}/`)))
      : new Set([...previous, ...folderPaths(tree).filter((folder) => !path || folder === path || folder.startsWith(`${path}/`))]));
  }

  function rowActions(target: FileTreeTarget, selection?: readonly FileTreeTarget[]): MenuGroups {
    const items = (selection ?? [target]).filter((item): item is CheckoutFavorite => item.kind !== "root");
    const allSaved = items.every(({ path }) => favoritePaths.has(path.toLowerCase()));
    return [...actions(target, selection), items.length ? [{
      label: selection ? (allSaved ? "Remove selected from favorites" : "Add selected to favorites") : (allSaved ? "Remove from favorites" : "Add to favorites"),
      run: () => setCheckoutFavorites(favoritesId, items.map((item) => ({ ...item, ignored: ignoredPaths.has(item.path) || undefined })), allSaved),
    }] : []];
  }

  const rootActions: MenuGroups = [...actions({ kind: "root", path: "" }), [
      { label: "Expand all folders", run: () => setSubtree("", false) },
      { label: "Collapse all folders", run: () => setSubtree("", true) },
    ], [
      { label: "Sort by", items: [
        { label: "Name ascending", disabled: preferences.sort === "ascending", run: () => setPreferences((value) => ({ ...value, sort: "ascending" })) },
        { label: "Name descending", disabled: preferences.sort === "descending", run: () => setPreferences((value) => ({ ...value, sort: "descending" })) },
        { label: "File type", disabled: preferences.sort === "type", run: () => setPreferences((value) => ({ ...value, sort: "type" })) },
      ] },
      { label: preferences.dotfiles ? "Hide dotfiles" : "Show dotfiles", run: () => {
        setManualReveal(undefined);
        setVisibilityOverride(undefined);
        setPreferences((value) => ({ ...value, dotfiles: !value.dotfiles }));
      } },
      ...(!simulated ? [
        { label: preferences.windowsHidden ? "Hide Windows hidden items" : "Show Windows hidden items", run: () => {
          setManualReveal(undefined);
          setVisibilityOverride(undefined);
          setPreferences((value) => ({ ...value, windowsHidden: !value.windowsHidden }));
        } },
        { label: preferences.ignored ? "Hide Git-ignored files" : "Show Git-ignored files",
          run: () => setPreferences((value) => ({ ...value, ignored: !value.ignored })) },
      ] : []),
      { label: preferences.autoReveal ? "Disable auto-reveal" : "Auto-reveal active file",
        run: () => { setManualReveal(undefined); setPreferences((value) => ({ ...value, autoReveal: !value.autoReveal })); } },
    ]];

  function openFavorite({ path, kind, ignored }: CheckoutFavorite) {
    if (kind === "file") onOpen(path);
    else {
      if (ignored && !simulated && !folders.includes(path)) {
        setPreferences((value) => ({ ...value, ignored: true }));
      }
      const reveal = { path, expand: true };
      setVisibilityOverride(reveal);
      setManualReveal(reveal);
    }
  }

  const visibleFavorites = favorites.filter(({ kind, path }) => workspace === "ide" || kind === "folder" || markdown(path));
  return <div ref={container} className="file-explorer-tree" onContextMenu={(event) => {
    if (event.target !== event.currentTarget) return;
    event.preventDefault();
    setBlankMenu({ x: event.clientX, y: event.clientY });
  }}>
    <ContextActions label="Checkout root" className="file-tree-root" groups={rootActions}>
      <button ref={rootButton} type="button" className="file-tree-row" onClick={(event) => event.currentTarget.focus()}>
        <FolderTree size={15} /><span>Checkout root</span>
      </button>
    </ContextActions>
    {visibleFavorites.length > 0 && <section className="file-tree-favorites" aria-label="Favorites">
      <h3>Favorites</h3>
      <ul className="file-tree">{visibleFavorites.map((item) => {
        const Icon = item.kind === "folder" ? Folder : markdown(item.path) ? FileText : FileCode2;
        return <li key={item.path}><ContextActions label={`Favorite ${item.path}`} groups={[[
          { label: item.kind === "folder" ? "Reveal favorite folder" : "Open favorite file", run: () => openFavorite(item) },
        ], ...rowActions(item)]}>
          <button type="button" className="file-tree-row" title={item.path} onClick={() => openFavorite(item)}>
            <Icon size={15} /><span>{fileName(item.path)}</span><small>{item.path}</small>
          </button>
        </ContextActions></li>;
      })}</ul>
    </section>}
    {ignored.pending && <p className="file-help" role="status">Loading Git-ignored files…</p>}
    {ignored.error && <p className="file-help" role="alert">{String(ignored.error)}</p>}
    {windowsHidden.pending && <p className="file-help" role="status">Checking Windows hidden items…</p>}
    {windowsHidden.error && <p className="file-help" role="alert">Could not check Windows hidden items: {String(windowsHidden.error)}</p>}
    {selection.length > 1 && <p className="file-selection-count" role="status">{selection.length} selected</p>}
    {!windowsHidden.pending && <FolderBranch tree={tree} parent="" rows={{ active, actions: rowActions, expanded, setSubtree, selected, choose,
      selection: (path) => selection.length > 1 && selected.has(path) ? selection : undefined, ignored: ignoredPaths, ignoredLoad: ignored.load,
      windowsHiddenLoad: windowsHidden.load }} />}
    <div className="file-tree-blank" aria-hidden="true" onContextMenu={(event) => {
      event.preventDefault();
      setBlankMenu({ x: event.clientX, y: event.clientY });
    }} />
    {blankMenu && <PositionedContextActions label="Checkout root" groups={rootActions} point={blankMenu}
      onClose={() => setBlankMenu(undefined)} restoreFocus={() => rootButton.current?.focus()} />}
  </div>;
}

export function FileCaption({ path, revision, actions, onReveal }: {
  path: string;
  revision: number | string;
  actions: (target: FileTreeTarget) => MenuGroups;
  onReveal: (path: string) => void;
}) {
  const label = typeof revision === "number" ? `r${revision}` : revision;
  const parts = path.split("/");
  return <div className="ide-file-caption">
    <nav className="file-breadcrumbs" aria-label="File breadcrumbs">
      <ol>{parts.map((name, index) => {
        const itemPath = parts.slice(0, index + 1).join("/");
        const last = index === parts.length - 1;
        const target: FileTreeTarget = { kind: last ? "file" : "folder", path: itemPath };
        return <li key={itemPath}>
          <ContextActions label={`Breadcrumb ${itemPath}`} title={itemPath} groups={actions(target)}>
            <button type="button" className="file-breadcrumb" aria-label={`Reveal ${last ? "file" : "folder"} ${name}`}
              aria-current={last ? "location" : undefined}
              onClick={() => onReveal(itemPath)}>{name}</button>
          </ContextActions>
        </li>;
      })}</ol>
    </nav>
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

export function FileDialog({ title, description, onClose, children, className = "", ownerPanel }: {
  title: string;
  description: string;
  onClose: () => void;
  children: ReactNode;
  className?: string;
  ownerPanel?: FilePanelApi;
}) {
  const ownerDocument = usePanelDocument(ownerPanel);
  const activator = useRef(ownerDocument.activeElement as HTMLElement | null);
  return (
    <Dialog.Root open onOpenChange={(open) => { if (!open) onClose(); }}>
      <Dialog.Portal container={ownerDocument.body}>
        <Dialog.Overlay className="file-dialog-overlay" />
        <Dialog.Content className={`file-dialog ${className}`} onCloseAutoFocus={(event) => {
          const element = activator.current;
          if (element?.isConnected) {
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
