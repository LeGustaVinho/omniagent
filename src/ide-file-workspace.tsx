import { invoke } from "@tauri-apps/api/core";
import {
  DockviewReact, themeDark, type DockviewApi, type DockviewReadyEvent,
  type IDockviewHeaderActionsProps, type IDockviewPanelHeaderProps, type IDockviewPanelProps, type IWatermarkPanelProps,
  type SerializedDockview,
} from "dockview-react";
import { FileCode2, FileText, Folder, FolderOpen, FolderTree, Hash, Link2, PanelLeftClose, Pin, X } from "lucide-react";
import { createContext, useCallback, useContext, useEffect, useRef, useState, type MouseEvent, type ReactNode, type Ref } from "react";
import { Dialog } from "radix-ui";
import { ContextActions, PositionedContextActions, type MenuAction, type MenuGroups } from "@/components/ui/context-actions";
import { fileName } from "@/ide-knowledge-data";
import { WorkbenchPanel } from "@/workbench-panel";
import { ControlTooltip } from "@/components/ui/control-tooltip";
import { checkoutFavoriteKey, isNoteFavorite, maxFavoriteGroupName, moveCheckoutFavorites, saveCheckoutFavoriteGroup,
  setCheckoutFavorites, useCheckoutFavorites, type CheckoutFavorite, type CheckoutFavoriteGroup,
  type CheckoutNoteFavorite, type CheckoutPathFavorite } from "@/checkout-favorites";
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
const groupPositions = [["left", "Left"], ["right", "Right"], ["top", "Above"], ["bottom", "Below"]] as const;
type DockTab = DockviewApi["panels"][number];
type CloseTab = (path: string, api: FilePanelApi) => boolean;
type ReopenFile = (path: string) => string | undefined | Promise<string | undefined>;

type FileDockCommands = {
  dirty: (path: string) => boolean;
  requestClose: CloseTab;
  loadClosedFile: ReopenFile;
  save: (path: string, ownerPanel: FilePanelApi) => Promise<boolean>;
};

type ClosedTab = { sequence: number; id: string; params: FilePanelParams; groupId: string; index: number };
type CloseBatch = { ids: string[]; savedOnly: boolean };
type GroupOption = { id: string; label: string };

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
  return (
    <FileDockContext.Provider value={context}>
      <div className="file-dock">
        <DockviewReact theme={themeDark} components={filePanelComponents} onReady={onReady}
          watermarkComponent={FileDockEmpty} defaultTabComponent={FileDockTab} rightHeaderActionsComponent={FileGroupControls}
          getTabContextMenuItems={() => []} disableFloatingGroups popoutUrl={editorPopoutPath} />
      </div>
    </FileDockContext.Provider>
  );
}

export function focusFile(dock: DockviewApi, path: string, view: FileView = "editor", home?: DockGroup) {
  const id = `${view}:${path}`;
  const existing = (home?.panels ?? dock.panels).find((panel) => panel.params?.path === path && panel.params?.view === view);
  if (existing) {
    existing.api.setActive();
    if (existing.group.api.location.type === "popout") existing.api.getWindow().focus();
    return existing;
  }
  return dock.addPanel({ id: unusedPanelId(dock, id), component: "file", params: { path, view }, title: fileName(path),
    position: home ? { referenceGroup: home } : undefined });
}

function groupName(dock: DockviewApi, group: DockGroup) {
  const path = (group.activePanel?.params as FilePanelParams | undefined)?.path;
  return `Group ${dock.groups.indexOf(group) + 1}${path ? ` · ${fileName(path)}` : ""}`;
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
  const [groupOptions, setGroupOptions] = useState<GroupOption[]>([]);
  const [savingGroup, setSavingGroup] = useState(false);
  const closeSequence = useRef(0);
  const pendingClose = useRef<CloseBatch>(null);
  const reopenPending = useRef(false);
  const savePending = useRef(false);
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
    const updateGroups = () => {
      const next = api.groups.filter((group) => group.api.isVisible).map((group) => ({ id: group.id, label: groupName(api, group) }));
      setGroupOptions((previous) => next.length === previous.length &&
        next.every(({ id, label }, index) => previous[index]?.id === id && previous[index]?.label === label) ? previous : next);
    };
    const activeChange = api.onDidActivePanelChange(() => { updateFile(); updateGroups(); save(); });
    const layoutChange = api.onDidLayoutChange(() => { updateGroups(); save(); });
    const cleanupPopouts = new Map<Window, () => void>();
    const addPopout = api.onDidAddPopoutGroup(({ group, window: popup }) => {
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
          updateGroups();
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

  const open = useCallback((file: string, groupId?: string) => {
    if (!api || !ready) return;
    const group = groupId === undefined ? undefined : api.groups.find((group) => group.id === groupId && group.api.isVisible);
    if (groupId !== undefined && !group) {
      setError("The selected editor group is no longer available.");
      return;
    }
    if (group) {
      if (api.panels.some((panel) => panel.params?.path === file && panel.group !== group)) {
        setError("The file is already open in another editor group.");
        return;
      }
      setError("");
    }
    focusFile(api, file, "editor", group);
  }, [api, ready]);

  function openGroupActions(path: string, openFile: (groupId: string) => void): MenuAction[] {
    if (groupOptions.length < 2 || api?.panels.some((panel) => panel.params?.path === path)) return [];
    return [{ label: "Open in editor group", items: groupOptions.map(({ id, label }) => ({
      label, run: () => openFile(id),
    })) }];
  }

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
    const home = api.panels.find((panel) => panel.params?.path === path && panel.params?.view === view)?.group
      ?? api.groups.find((group) => group.api.location.type === "grid");
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
    const dirtyEditors = group.panels.some(({ params }) =>
      params?.view === "editor" && typeof params.path === "string" && commands.dirty(params.path));
    const others = api.groups.filter((other) => other.id !== group.id);
    const gridNeighbors = group.api.location.type === "grid" ? others.filter((other) => other.api.location.type === "grid") : [];
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
      ...(gridNeighbors.length ? [{ label: "Move group in grid", items: gridNeighbors.map((other) => ({
        label: groupName(api, other),
        items: groupPositions.map(([position, label]) => ({
          label,
          run: () => {
            if (api.groups.includes(group) && api.groups.includes(other) && group.api.location.type === "grid" && other.api.location.type === "grid") {
              group.api.moveTo({ group: other, position });
            }
          },
        })),
      })) }] : []),
      ...(others.length ? [{ label: "Merge this group into", items: others.map((other) => ({
        label: groupName(api, other), run: () => mergeGroups(group, other),
      })) }, { label: "Merge all groups here", run: () => {
        for (const other of others) mergeGroups(other, group);
      } }] : []),
    ], [
      { label: "Save all tabs in group", disabled: savingGroup || !dirtyEditors, run: () => void saveGroup(group, commands) },
      { label: "Close saved tabs in group", disabled: !saved.length, run: () => closeTabs(saved, commands, true) },
      { label: unpinned.length === group.panels.length ? "Close editor group" : "Close unpinned tabs in group",
        disabled: !unpinned.length, run: () => closeTabs(unpinned, commands) },
    ], reopenActions(commands, group.id)];
  }

  async function saveGroup(group: DockGroup, { dirty, save }: FileDockCommands) {
    if (!api?.groups.includes(group) || savePending.current) return;
    const panels = [...group.panels];
    const attempted = new Set<string>();
    savePending.current = true;
    setSavingGroup(true);
    try {
      for (const panel of panels) {
        if (liveDock.current !== api) break;
        const params = panel.params as FilePanelParams | undefined;
        if (api.getPanel(panel.id) !== panel || !params || params.view !== "editor" || attempted.has(params.path) || !dirty(params.path)) continue;
        attempted.add(params.path);
        if (!await save(params.path, panel.api)) break;
      }
    } catch (failure) {
      if (liveDock.current === api) setError(String(failure));
    } finally {
      savePending.current = false;
      if (liveDock.current === api) setSavingGroup(false);
    }
  }

  function mergeGroups(source: DockGroup, target: DockGroup) {
    if (api?.groups.includes(source) && api.groups.includes(target)) {
      source.api.moveTo({ group: target, position: "center" });
    }
  }

  function layoutActions(tab: FilePanelApi, commands: FileDockCommands): MenuGroups {
    if (!api) return [];
    const { group } = tab;
    const index = group.panels.findIndex(({ id }) => id === tab.id);
    const otherTabs = group.panels.filter((panel) => panel.id !== tab.id && !panel.params?.pinned);
    const right = index === -1 ? [] : group.panels.slice(index + 1).filter((panel) => !panel.params?.pinned);
    const saved = otherTabs.filter((panel) => !commands.dirty(String(panel.params?.path)));
    const others = api.groups.filter((other) => other.id !== group.id);
    const main = others.find((other) => other.api.location.type === "grid" && other.panels.length);
    return [[
      { label: "Move tab", items: [
        { label: "New group to the right", disabled: group.panels.length < 2, run: () => tab.moveTo({ group, position: "right" }) },
        { label: "New group below", disabled: group.panels.length < 2, run: () => tab.moveTo({ group, position: "bottom" }) },
        ...others.map((other) => ({ label: groupName(api, other), run: () => tab.moveTo({ group: other, position: "center" }) })),
      ] },
      ...(group.api.location.type === "popout" ? [
        ...(main && group.panels.length > 1 ? [{ label: "Move tab to main window", run: () => tab.moveTo({ group: main, position: "center" }) }] : []),
        { label: "Return editor window to main window", run: () => group.api.getWindow().close() },
      ] : [{ label: "Move tab to new window", run: () => {
        const selected = api.getPanel(tab.id);
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

  return { api, error, onReady, open, openGroupActions, openInWindow, path: file?.path, view: file?.view, ready, split, reconcilePaths,
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
  onOpenNoteFavorite?: (target: CheckoutNoteFavorite) => void;
  actions: TreeRows["actions"];
  reveal?: TreeReveal;
  checkoutId: string;
  workspace: "ide" | "knowledge";
  simulated: boolean;
};

type FavoriteGroupEdit = { group?: CheckoutFavoriteGroup; items?: readonly CheckoutFavorite[] };

export function FileTree(props: FileTreeProps) {
  const { checkoutId, workspace, simulated } = props;
  const storageKey = `omniagent:file-tree:${workspace}:${simulated ? "example" : encodeURIComponent(checkoutId)}`;
  return <PersistentFolderTree key={storageKey} {...props} storageKey={storageKey} />;
}

function PersistentFolderTree({ paths, folders = [], active, onOpen, onOpenNoteFavorite, actions, reveal, storageKey, checkoutId, workspace, simulated }:
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
  const { favorites, groups: favoriteGroups } = useCheckoutFavorites(favoritesId);
  const [favoriteGroupEdit, editFavoriteGroup] = useState<FavoriteGroupEdit>();
  const [collapsedFavoriteGroups, setCollapsedFavoriteGroups] = useState(new Set<string>());
  const newFavoriteGroup = { label: "New favorite group…", run: () => editFavoriteGroup({}) };
  const favoriteKeys = new Set(favorites.map(checkoutFavoriteKey));
  const automaticReveal: TreeReveal | undefined = preferences.autoReveal && active ? { path: active } : undefined;
  const revealPath = manualReveal?.path ?? automaticReveal?.path;
  const visibleReveals = [manualReveal ?? visibilityOverride, automaticReveal].filter((reveal) => reveal !== undefined);
  const scopedIgnored = preferences.ignored ? undefined : visibleReveals.find((reveal) => reveal.includeIgnored)?.path;
  const ignored = useIgnoredTree(checkoutId, !simulated && (preferences.ignored || Boolean(scopedIgnored)), expanded, preferences.dotfiles, visibleReveals, scopedIgnored);
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
    if (revealPath === undefined) return;
    if (!manualReveal && lastAutoReveal.current === revealPath) return;
    const frame = requestAnimationFrame(() => {
      const button = revealPath === "" ? rootButton.current
        : [...(container.current?.querySelectorAll<HTMLButtonElement>("[data-tree-item]") ?? [])].find((item) => item.title === revealPath);
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
    const items = (selection ?? [target]).filter((item): item is CheckoutPathFavorite => item.kind !== "root");
    const allSaved = items.every((item) => favoriteKeys.has(checkoutFavoriteKey(item)));
    return [...actions(target, selection), items.length ? [{
      label: selection ? (allSaved ? "Remove selected from favorites" : "Add selected to favorites") : (allSaved ? "Remove from favorites" : "Add to favorites"),
      run: () => setCheckoutFavorites(favoritesId, items.map((item) => ({ ...item, ignored: ignoredPaths.has(item.path) || undefined })), allSaved),
    }] : []];
  }

  const rootActions: MenuGroups = [...actions({ kind: "root", path: "" }), [
      { label: "Reveal active file", disabled: !active, run: () => {
        if (!active) return;
        const reveal = { path: active };
        setManualReveal(reveal);
        setVisibilityOverride(reveal);
      } },
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
    ], [newFavoriteGroup]];

  function openFavorite(item: CheckoutFavorite) {
    if (isNoteFavorite(item)) { onOpenNoteFavorite?.(item); return; }
    const { path, kind, ignored } = item;
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

  const visibleFavorites = favorites.filter((item) => isNoteFavorite(item)
    ? workspace === "knowledge" && onOpenNoteFavorite && markdown(item.path)
    : workspace === "ide" || item.kind === "folder" || markdown(item.path));
  function favoriteRows(items: readonly CheckoutFavorite[]) {
    if (!items.length) return null;
    return <ul className="file-tree">{items.map((item) => {
      const noteTarget = isNoteFavorite(item);
      const Icon = noteTarget ? (item.kind === "heading" ? Hash : Link2)
        : item.kind === "folder" ? Folder : markdown(item.path) ? FileText : FileCode2;
      const duplicate = noteTarget && item.occurrence > 0 ? " · Occurrence " + (item.occurrence + 1) : "";
      const name = noteTarget ? item.label + duplicate : fileName(item.path);
      const location = noteTarget ? item.path + "#" + item.label + duplicate : item.path;
      const openLabel = noteTarget ? (item.kind === "heading" ? "Open favorite heading" : "Open favorite block")
        : item.kind === "folder" ? "Reveal favorite folder" : "Open favorite file";
      return <li key={checkoutFavoriteKey(item)}><ContextActions label={"Favorite " + location} groups={[[
        { label: openLabel, run: () => openFavorite(item) },
        ...(noteTarget ? [{ label: "Open note", run: () => onOpen(item.path) }] : []),
        { label: "Move to favorite group", items: [
          ...favoriteGroups.map((group) => ({ label: "Move to " + group.name, disabled: item.groupId === group.id,
            run: () => moveCheckoutFavorites(favoritesId, [item], group.id) })),
          { label: "Remove from group", disabled: !item.groupId, run: () => moveCheckoutFavorites(favoritesId, [item]) },
          { label: newFavoriteGroup.label, run: () => editFavoriteGroup({ items: [item] }) },
        ] },
      ], ...(noteTarget ? [[{ label: "Remove from favorites", run: () => setCheckoutFavorites(favoritesId, [item], true) }]] : rowActions(item))]}>
        <button type="button" className="file-tree-row" title={location} onClick={() => openFavorite(item)}>
          <Icon size={15} /><span>{name}</span><small>{item.path}</small>
        </button>
      </ContextActions></li>;
    })}</ul>;
  }
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
    {(visibleFavorites.length > 0 || favoriteGroups.length > 0) && <section className="file-tree-favorites" aria-label="Favorites">
      <ContextActions label="Favorites" groups={[[newFavoriteGroup]]}><h3>Favorites</h3></ContextActions>
      {favoriteGroups.map((group) => {
        const open = !collapsedFavoriteGroups.has(group.id);
        const Icon = open ? FolderOpen : Folder;
        const toggleGroup = () => setCollapsedFavoriteGroups((previous) => {
          const next = new Set(previous);
          if (!next.delete(group.id)) next.add(group.id);
          return next;
        });
        return <section key={group.id} className="file-favorite-group" aria-label={`Favorite group ${group.name}`}>
          <ContextActions label={`Favorite group ${group.name}`} groups={[[
            { label: open ? "Collapse favorite group" : "Expand favorite group", run: toggleGroup },
            { label: "Rename favorite group…", run: () => editFavoriteGroup({ group }) },
          ]]}>
            <button type="button" className="file-tree-row" title={group.name} aria-expanded={open} onClick={toggleGroup}>
              <Icon size={15} /><span>{group.name}</span>
            </button>
          </ContextActions>
          {open && favoriteRows(visibleFavorites.filter(({ groupId }) => groupId === group.id))}
        </section>;
      })}
      {favoriteRows(visibleFavorites.filter(({ groupId }) => !groupId))}
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
    {favoriteGroupEdit && <FavoriteGroupDialog checkoutId={favoritesId} edit={favoriteGroupEdit} close={() => editFavoriteGroup(undefined)} />}
  </div>;
}

function FavoriteGroupDialog({ checkoutId, edit, close }: { checkoutId: string; edit: FavoriteGroupEdit; close: () => void }) {
  const [name, setName] = useState(edit.group?.name ?? "");
  const [error, setError] = useState("");
  return <FileDialog title={edit.group ? "Rename favorite group" : "New favorite group"}
    description={edit.group ? edit.group.name : "Groups organize favorites in this worktree."} onClose={close}>
    <form className="grid gap-2" onSubmit={(event) => {
      event.preventDefault();
      try {
        saveCheckoutFavoriteGroup(checkoutId, { id: edit.group?.id, name, items: edit.items });
        close();
      } catch (failure) {
        setError(failure instanceof Error ? failure.message : String(failure));
      }
    }}>
      <label className="file-field">Group name
        <input autoFocus value={name} maxLength={maxFavoriteGroupName} onChange={(event) => {
          setName(event.currentTarget.value);
          setError("");
        }} />
      </label>
      {error && <p role="alert" className="file-help">{error}</p>}
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" onClick={close}>Cancel</button>
        <button type="submit" className="fw-btn fw-primary" disabled={!name.trim()}>{edit.group ? "Rename group" : "Create group"}</button>
      </div>
    </form>
  </FileDialog>;
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

export function FileSidebar({ name, drawer, open, onClose, children }: {
  name: string; drawer: boolean; open: boolean; onClose: () => void; children: ReactNode;
}) {
  return <WorkbenchPanel as="aside" drawer={drawer} open={open} label={name} onClose={onClose}
    drawerClass="file-workspace-drawer" className="file-workspace-sidebar" aria-label={name}
    edge="right" resizeKey={name} resizeLabel={name}>
    {drawer && <header className="work-panel-header">
      <span>{name}</span>
      <ControlTooltip label={`Close ${name}`}><button className="fw-icon" type="button" aria-label={`Close ${name}`}
        onClick={onClose}><X size={16} aria-hidden="true" /></button></ControlTooltip>
    </header>}
    {children}
  </WorkbenchPanel>;
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

export function SidebarToggle({ open, setOpen, label, buttonRef }: {
  open: boolean; setOpen: (open: boolean) => void; label: string; buttonRef?: Ref<HTMLButtonElement>;
}) {
  return (
    <ControlTooltip label={label}><button ref={buttonRef} type="button" className="fw-icon" aria-label={label} aria-expanded={open}
      onClick={() => setOpen(!open)}><PanelLeftClose size={16} aria-hidden="true" /></button></ControlTooltip>
  );
}

export function useFileSidebar(width: number) {
  const drawer = width < 840;
  const [preference, setPreference] = useState<boolean>();
  return [preference ?? !drawer, setPreference, drawer] as const;
}
