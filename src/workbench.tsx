import {
  Activity, ArrowLeft, Blocks, BookOpen, ClipboardList, Code2, GitBranch, ListChecks, MessageSquare,
  PanelLeft, PanelLeftClose, PanelLeftOpen, Search, Settings, type LucideIcon,
} from "lucide-react";
import { lazy, Suspense, useEffect, useRef, useState, type ComponentType, type ReactNode } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { CheckoutSelector } from "@/checkout-selector";
import { Button } from "@/components/ui/button";
import { ControlTooltip } from "@/components/ui/control-tooltip";
import { ConversationsWorkspace, DiagnosticsDock } from "@/desktop-dock";
import { openWorkspaceFile } from "@/file-context-actions";
import type { FileLinkTarget } from "@/file-links";
import { RunInspector } from "@/inspection-panel";
import { InspectorTabs } from "@/inspector-tabs";
import { PanelResizeScope } from "@/panel-resize";
import { useRunSubscription } from "@/run-projection";
import { RunInspectorNavigation } from "@/run-inspector-navigation";
import { GoTo, type Destination } from "@/workbench-navigation";
import { useWorkspaceSize, WorkbenchPanel, WorkspaceDrawersSuspended, WorkspaceVisibility } from "@/workbench-panel";
import type { QuestionFocus, QueueJobFocus, WorkspaceDestination, WorkspaceId, WorkspaceProps } from "@/workbench-contract";

const SettingsDialog = lazy(() => import("@/settings-dialog"));
const QueuePanel = lazy(() => import("@/queue-live"));
const ChangesPanel = lazy(() => import("@/changes-panel"));
const workspaceComponents: Partial<Record<WorkspaceId, ComponentType<WorkspaceProps>>> = {
  ide: lazy(() => import("@/ide-workspace")),
  tasks: lazy(() => import("@/tasks-workspace")),
  pipelines: lazy(() => import("@/pipelines-workspace")),
  knowledge: lazy(() => import("@/knowledge-workspace")),
  git: lazy(() => import("@/git-live-workspace")),
};
const areas = [
  { id: "conversations", label: "Conversations", icon: MessageSquare },
  { id: "ide", label: "IDE", icon: Code2 },
  { id: "tasks", label: "Tasks", icon: ClipboardList },
  { id: "pipelines", label: "Pipelines", icon: Blocks },
  { id: "knowledge", label: "Knowledge", icon: BookOpen },
  { id: "git", label: "Git", icon: GitBranch },
] as const;

type Origin = {
  area: WorkspaceId;
  target?: string;
  trigger: HTMLElement | null;
  inspector: SidePane | null;
  queue: boolean;
  scroll: { element: HTMLElement; top: number; left: number }[];
};
type Navigation = {
  active: WorkspaceId;
  visited: Partial<Record<WorkspaceId, { target?: string }>>;
  trail: Origin[];
};
type SidePane = { type: "activity"; runId: string; conversationId?: string; focusQuestion?: QuestionFocus }
  | { type: "changes"; target: "saved" | "working" };

function show(workspace: WorkspaceId, target?: string, prior?: Navigation, trail = prior?.trail ?? []): Navigation {
  const destination = target === undefined ? prior?.visited[workspace] ?? {} : { target };
  return { active: workspace, visited: { ...prior?.visited, [workspace]: destination }, trail };
}

function areaLabel(workspace: WorkspaceId) {
  return areas.find(({ id }) => id === workspace)?.label ?? "Diagnostics";
}

function scrollPositions(view: HTMLElement | null | undefined, trigger: HTMLElement | null) {
  const elements = new Set(view?.querySelectorAll<HTMLElement>(
    ".conv-history, .conv-library-entries, .delivery-main, .delivery-detail-content, .file-sidebar-scroll, .note-reading, .pipeline-view",
  ));
  for (let element = trigger?.parentElement; element && element !== document.body; element = element.parentElement) {
    elements.add(element);
  }
  return [...elements].filter((element) => element.scrollTop || element.scrollLeft)
    .map((element) => ({ element, top: element.scrollTop, left: element.scrollLeft }));
}

function readNavigation(key: string): Navigation {
  try {
    const stored = localStorage.getItem(key);
    if (stored === "diagnostics") return show(stored);
    const area = areas.find(({ id }) => id === stored);
    if (area) return show(area.id);
  } catch {
    // The workbench still opens if local UI storage is unavailable.
  }
  return show("conversations");
}

function useMedia(query: string) {
  const [matches, setMatches] = useState(() => window.matchMedia?.(query).matches ?? false);
  useEffect(() => {
    const media = window.matchMedia?.(query);
    if (!media) return;
    const update = () => setMatches(media.matches);
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, [query]);
  return matches;
}

const loading = <p className="work-progress" role="status">Loading workspace…</p>;

function RailButton({ label, text = label, icon: Icon, current, pressed, onClick }: {
  label: string;
  text?: string;
  icon: LucideIcon;
  current?: boolean;
  pressed?: boolean;
  onClick: () => void;
}) {
  return <ControlTooltip label={label} side="right">
      <button type="button" className="work-rail-button" aria-label={label} aria-current={current ? "page" : undefined}
        aria-pressed={pressed} onClick={onClick}><Icon size={16} aria-hidden="true" /><span>{text}</span></button>
  </ControlTooltip>;
}

export default function Workbench({ checkout, openRepository, onSelectCheckout, initialConversationId, destination, fileTarget }: {
  checkout: ProjectCheckout;
  openRepository: ReactNode;
  onSelectCheckout: (checkout: ProjectCheckout, conversationId?: string, destination?: WorkspaceDestination) => Promise<void>;
  initialConversationId?: string;
  destination?: WorkspaceDestination;
  fileTarget?: FileLinkTarget;
}) {
  const lastAreaKey = "omniagent-workspace-area:" + checkout.checkoutId;
  const [nav, setNavigation] = useState(() => {
    const prior = readNavigation(lastAreaKey);
    if (destination) return show(destination.workspace, destination.target, prior);
    return initialConversationId ? show("conversations", initialConversationId, prior) : prior;
  });
  const [settingsPage, setSettingsPage] = useState<string>();
  const [settingsVisited, setSettingsVisited] = useState(false);
  const [side, setSide] = useState<SidePane | null>(null);
  const [closingInspector, setClosingInspector] = useState(false);
  const [queueState, setQueueState] = useState<"open" | "closed">();
  const [queueFocus, setQueueFocus] = useState<QueueJobFocus>();
  const [sidebarOverride, setSidebarOverride] = useState<boolean>();
  const [collapsed, setCollapsed] = useState(false);
  const [goTo, setGoTo] = useState(false);
  const goToOrigin = useRef<HTMLElement | null>(null);
  const goToButton = useRef<HTMLButtonElement>(null);
  const workViews = useRef<HTMLDivElement>(null);
  const workBody = useRef<HTMLDivElement>(null);
  const queueTrigger = useRef<HTMLElement | null>(null);
  const sideTrigger = useRef<HTMLElement | null>(null);
  const sidebarTrigger = useRef<HTMLButtonElement>(null);
  const narrowRail = useMedia("(width < 1040px)");
  const size = useWorkspaceSize(workBody);
  const availableWidth = size.width - (narrowRail || collapsed ? 48 : 156);
  const compact = useWorkspaceSize(workViews).width < 840;
  const queueDrawer = availableWidth < 900 || size.height < 600;
  const { error, reconnect } = useRunSubscription(checkout);

  useEffect(() => {
    if (!fileTarget) return;
    setSettingsPage(undefined);
    openWorkspaceFile((workspace, target) => setNavigation((prior) => show(workspace, target, prior)),
      fileTarget.path, fileTarget.line ?? undefined, { column: fileTarget.column ?? undefined, repeat: true });
  }, [fileTarget]);

  useEffect(() => {
    function shortcut(event: globalThis.KeyboardEvent) {
      if (!event.ctrlKey || event.altKey || event.shiftKey || event.metaKey || event.repeat || event.isComposing ||
        event.defaultPrevented || event.key.toLowerCase() !== "k") return;
      const focused = document.activeElement;
      if (focused instanceof HTMLElement && (focused.isContentEditable ||
        focused.closest('input, textarea, select, .monaco-editor, .cm-editor, .xterm, [role="dialog"], [role="menu"]'))) return;
      if (goTo || settingsPage) return;
      event.preventDefault();
      goToOrigin.current = focused instanceof HTMLElement && focused !== document.body ? focused : null;
      setGoTo(true);
    }
    window.addEventListener("keydown", shortcut);
    return () => window.removeEventListener("keydown", shortcut);
  }, [goTo, settingsPage]);

  function closeGoTo() {
    setGoTo(false);
    requestAnimationFrame(() => {
      const trigger = goToOrigin.current;
      if (trigger?.getClientRects().length) trigger.focus({ preventScroll: true });
      else goToButton.current?.focus({ preventScroll: true });
    });
  }

  function showSettings(page: string) {
    setSettingsVisited(true);
    setSettingsPage(page);
  }

  useEffect(() => {
    try {
      localStorage.setItem(lastAreaKey, nav.active);
    } catch {
      // Navigation remains available in memory.
    }
  }, [lastAreaKey, nav.active]);

  function navigateArea(destination: WorkspaceId) {
    setNavigation((prior) => show(destination, undefined, prior, []));
  }

  function restoreWorkspaceFocus(trigger: HTMLElement | null, framesLeft = 4) {
    if (trigger?.getClientRects().length) {
      trigger.focus({ preventScroll: true });
      return;
    }
    if (framesLeft && trigger?.isConnected) {
      // A workspace can reattach the originating drawer after the inspector releases its width.
      requestAnimationFrame(() => restoreWorkspaceFocus(trigger, framesLeft - 1));
      return;
    }
    const view = workViews.current?.querySelector('.work-deck .work-view:not([hidden])');
    const heading = Array.from(view?.querySelectorAll<HTMLElement>('h1, h2, h3') ?? [])
      .find((element) => element.getClientRects().length);
    if (heading) {
      if (!heading.hasAttribute('tabindex')) heading.tabIndex = -1;
      heading.focus({ preventScroll: true });
    } else document.querySelector<HTMLElement>('.work-rail-button[aria-current="page"]')?.focus({ preventScroll: true });
  }

  function goBack() {
    const origin = nav.trail.at(-1);
    if (!origin) return;
    setNavigation((prior) => ({
      ...prior,
      active: origin.area,
      visited: { ...prior.visited, [origin.area]: { target: origin.target } },
      trail: prior.trail.slice(0, -1),
    }));
    setSide(origin.inspector);
    if (origin.queue) setQueueState("open");
    requestAnimationFrame(() => {
      for (const { element, top, left } of origin.scroll) {
        if (element.isConnected) { element.scrollTop = top; element.scrollLeft = left; }
      }
      restoreWorkspaceFocus(origin.trigger);
    });
  }

  function workspaceContext(): WorkspaceProps {
    function showSide(next: SidePane) {
      const active = document.activeElement;
      if (active instanceof HTMLElement && !active.closest(".work-inspector")) sideTrigger.current = active;
      const existing = side;
      if (next.type === "activity" && !next.focusQuestion && existing?.type === "activity" && next.runId === existing.runId) {
        requestAnimationFrame(() => document.querySelector<HTMLElement>('.work-inspector [aria-label="Close activity"]')?.focus());
        return;
      }
      setSide(next);
    }
    return {
      checkout,
      openWorkspace: (workspace, target) => {
        const inspector = side;
        const focused = document.activeElement;
        const trigger = focused instanceof HTMLElement && focused !== document.body ? focused : null;
        const jumping = nav.active !== workspace;
        const fromQueue = Boolean(trigger?.closest(".work-queue-panel"));
        const scroll = jumping ? scrollPositions(workViews.current?.querySelector<HTMLElement>(".work-deck .work-view:not([hidden])"), trigger) : [];
        if (jumping && inspector) setSide(null);
        if (jumping && fromQueue && queueDrawer) setQueueState("closed");
        setNavigation((prior) => show(workspace, target, prior, jumping ? [...prior.trail, {
          area: prior.active,
          target: prior.visited[prior.active]?.target,
          trigger,
          inspector,
          queue: fromQueue,
          scroll,
        }] : prior.trail));
      },
      openActivity: (runId, conversationId, requestId) => showSide({
        type: "activity", runId, conversationId, focusQuestion: requestId === undefined ? undefined : { requestId },
      }),
      openCheckoutWorkspace: async (selected, destination) => {
        if (selected.projectId !== checkout.projectId) throw new Error("This worktree is unavailable in the current project.");
        if (selected.checkoutId === checkout.checkoutId) setNavigation((current) => show(destination.workspace, destination.target, current));
        else await onSelectCheckout(selected, undefined, destination);
      },
      openChanges: (target) => showSide({ type: "changes", target }),
      openQueueJob: (jobId) => {
        rememberQueueTrigger();
        if (queueDrawer) setSide(null);
        setQueueFocus({ projectId: checkout.projectId, jobId });
        setQueueState("open");
      },
    };
  }

  const workspace = workspaceContext();
  const sideDrawer = availableWidth < (side?.type === "changes" ? 990 : 840);
  const sidebar = sidebarOverride ?? !compact;
  function closeSide() {
    setClosingInspector(true);
    setSide(null);
    const trigger = sideTrigger.current;
    // Keep child drawers suspended until they can measure the width released by the inspector.
    requestAnimationFrame(() => requestAnimationFrame(() => {
      setClosingInspector(false);
      requestAnimationFrame(() => restoreWorkspaceFocus(trigger));
    }));
  }

  function changeSidebar(visible: boolean) {
    setSidebarOverride(visible);
    if (!visible && compact) requestAnimationFrame(() => sidebarTrigger.current?.focus());
  }
  const queue = queueState === "open";
  function rememberQueueTrigger() {
    const active = document.activeElement;
    if (active instanceof HTMLElement && !active.closest(".work-queue-panel")) queueTrigger.current = active;
  }
  function showQueue() {
    rememberQueueTrigger();
    if (queueDrawer) {
      setSide(null);
    }
    setQueueState("open");
  }
  function closeQueue() {
    setQueueState("closed");
    requestAnimationFrame(() => {
      const trigger = queueTrigger.current;
      if (trigger?.getClientRects().length) trigger.focus({ preventScroll: true });
      else document.querySelector<HTMLElement>('.work-rail-button[aria-label="Queue"]')?.focus({ preventScroll: true });
    });
  }
  const currentArea = areaLabel(nav.active);
  const origin = nav.trail.at(-1);
  const destinations: Destination[] = [
    ...areas.map(({ id, label, icon }) => ({ label, icon, current: nav.active === id, select: () => navigateArea(id) })),
    { label: "Queue", icon: ListChecks, select: showQueue },
    { label: "Diagnostics", icon: Activity, current: nav.active === "diagnostics", select: () => navigateArea("diagnostics") },
    { label: "Settings", icon: Settings, select: () => showSettings("appearance") },
  ];
  const navigationSearch = <ControlTooltip label="Go to…" shortcut="Ctrl+K" side={narrowRail ? "right" : "bottom"}>
    <Button type="button" ref={goToButton} className={narrowRail ? "work-goto-compact" : "work-goto-trigger"} variant="outline"
      aria-label="Go to…" aria-haspopup="dialog" onClick={(event) => {
        goToOrigin.current = event.currentTarget;
        setGoTo(true);
      }}>
      <Search size={16} aria-hidden="true" />
      {!narrowRail && <><span>Go to…</span><kbd>Ctrl+K</kbd></>}
    </Button>
  </ControlTooltip>;

  return (
    <div className="work-shell flex min-h-0 flex-1 flex-col" data-rail-compact={narrowRail || collapsed}>
      <header className="work-titlebar">
        <div className="work-wordmark"><span className="work-mark" aria-hidden="true"><Blocks size={15} /></span><span className="work-brand-name">OmniAgent</span></div>
        <div className="work-breadcrumb">
          {origin && <ControlTooltip label={`Back to ${areaLabel(origin.area)}`}>
            <Button type="button" className="ui-icon-button" variant="ghost" aria-label={`Back to ${areaLabel(origin.area)}`} onClick={goBack}>
              <ArrowLeft size={16} aria-hidden="true" />
            </Button>
          </ControlTooltip>}
          {nav.active === "conversations" && <ControlTooltip label="Toggle conversation navigation">
            <Button ref={sidebarTrigger} variant="ghost" className="ui-icon-button" aria-label="Toggle conversation navigation" aria-expanded={sidebar}
              onClick={() => changeSidebar(!sidebar)}><PanelLeft size={16} aria-hidden="true" /></Button>
          </ControlTooltip>}
          <CheckoutSelector checkout={checkout}
            onSelect={async (selected, shortcut) => {
              if (shortcut && selected.checkoutId === checkout.checkoutId) workspace.openWorkspace(shortcut.workspace, shortcut.target);
              else await onSelectCheckout(selected, undefined, shortcut);
            }} />
          <span className="work-crumb-divider" aria-hidden="true">/</span><span className="work-current-area">{currentArea}</span>
        </div>
        {!narrowRail && navigationSearch}
        <div className="flex shrink-0 items-center gap-2">{openRepository}</div>
      </header>
      {error && (
        <div className="work-banner" role="alert"><span>{error}</span><Button variant="outline" onClick={reconnect}>Reconnect</Button></div>
      )}
      <div className="work-body" ref={workBody}>
        <aside aria-label="Workspace" className="work-rail">
          {narrowRail ? navigationSearch : <RailButton label={collapsed ? "Expand navigation" : "Collapse navigation"}
            text="Collapse" icon={collapsed ? PanelLeftOpen : PanelLeftClose} onClick={() => setCollapsed(!collapsed)} />}
          <nav aria-label="Workspaces" className="grid gap-1">
            {areas.map(({ id, icon, label }) => <RailButton key={id} label={label} icon={icon} current={nav.active === id}
              onClick={() => navigateArea(id)} />)}
          </nav>
          <div className="work-rail-end">
            <RailButton label="Queue" icon={ListChecks} pressed={queue} onClick={queue ? closeQueue : showQueue} />
            <RailButton label="Diagnostics" icon={Activity} current={nav.active === "diagnostics"} onClick={() => navigateArea("diagnostics")} />
            <RailButton label="Settings" icon={Settings} onClick={() => showSettings("appearance")} />
          </div>
        </aside>
        <div className="work-decks" ref={workViews}>
          <WorkspaceDrawersSuspended.Provider value={Boolean(side || closingInspector || queue && queueDrawer || settingsPage || goTo)}>
            <div className="work-deck">
              <RunInspectorNavigation.Provider value={{ openActivity: workspace.openActivity, openWorkspace: workspace.openWorkspace, activeRunId: side?.type === "activity" ? side.runId : undefined,
                openGitAttempt: (runId) => workspace.openWorkspace("git", `attempt:${runId}`), openProviders: () => showSettings("providers") }}>
                <WorkspaceViews nav={nav} context={workspace} sidebar={sidebar && !(compact && (side || queueDrawer && queue))} onSidebarChange={changeSidebar} compact={compact}
                  onOpenOtherCheckout={onSelectCheckout} openSystem={() => showSettings("system")} />
              </RunInspectorNavigation.Provider>
            </div>
          </WorkspaceDrawersSuspended.Provider>
          {queueState && <WorkbenchPanel className="work-queue-panel" edge="top" resizeKey="queue" resizeLabel="queue"
            drawer={queueDrawer} drawerClass="work-queue-drawer" label="Queue" open={queueState === "open" && !(queueDrawer && side)} onClose={closeQueue}>
            <Suspense fallback={loading}><QueuePanel {...workspace} focusJob={queueFocus}
              onOpenRegisteredWorktree={(source, destination) => onSelectCheckout(source, undefined, destination)}
              active={queueState === "open"} onClose={closeQueue} /></Suspense>
          </WorkbenchPanel>}
        </div>
        {side && <SideInspector key={side.type === "activity" ? side.runId : side.target} side={side} compact={sideDrawer} close={closeSide}>
          <Suspense fallback={loading}>
            {side.type === "activity" ? <RunInspector {...workspace} runId={side.runId} conversationId={side.conversationId}
              focusQuestion={side.focusQuestion} onClose={closeSide} /> : <PanelResizeScope scope="changes-inspector">
              <ChangesPanel {...workspace} target={side.target} />
            </PanelResizeScope>}
          </Suspense>
        </SideInspector>}
      </div>
      {settingsVisited && <Suspense fallback={null}>
        <SettingsDialog {...workspace} open={Boolean(settingsPage)} initialPage={settingsPage}
          onOpenChange={(open) => { if (!open) setSettingsPage(undefined); }} />
      </Suspense>}
      {goTo && <GoTo destinations={destinations} onClose={closeGoTo} />}
    </div>
  );
}

function WorkspaceViews({ nav, context, sidebar, onSidebarChange, compact, openSystem, onOpenOtherCheckout }: {
  nav: Navigation;
  context: WorkspaceProps;
  sidebar: boolean;
  onSidebarChange: (visible: boolean) => void;
  compact: boolean;
  openSystem?: () => void;
  onOpenOtherCheckout?: (checkout: ProjectCheckout, conversationId?: string, destination?: WorkspaceDestination) => Promise<void>;
}) {
  return Object.entries(nav.visited).map(([id, state]) => {
    const workspace = id as WorkspaceId;
    const View = workspaceComponents[workspace];
    const visible = nav.active === workspace;
    const target = state?.target;
    return <div key={id} className="work-view" hidden={!visible}>
      <WorkspaceVisibility.Provider value={visible}><Suspense fallback={loading}>
        {workspace === "conversations" ? <ConversationsWorkspace checkout={context.checkout} target={target} sidebar={visible && sidebar} onSidebarChange={onSidebarChange}
            compact={compact} onOpenOtherCheckout={onOpenOtherCheckout} onOpenCheckoutInIde={async (source) => {
              if (source.checkoutId === context.checkout.checkoutId) context.openWorkspace("ide");
              else if (onOpenOtherCheckout) await onOpenOtherCheckout(source, undefined, { workspace: "ide" });
              else throw new Error("This worktree cannot be opened from the current workspace.");
            }} />
          : workspace === "diagnostics" ? <DiagnosticsDock checkout={context.checkout} openSystem={openSystem} />
            : View && <View {...context} target={target} />}
      </Suspense></WorkspaceVisibility.Provider>
    </div>;
  });
}

function SideInspector({ side, compact, close, children }: { side: SidePane; compact: boolean; close: () => void; children: ReactNode }) {
  const [expanded, setExpanded] = useState(false);
  const title = side.type === "changes" ? "Changes" : "Activity";
  const tabs = <InspectorTabs title={title} content={children} expanded={expanded} onExpand={() => setExpanded(!expanded)} onClose={close} />;
  return <WorkbenchPanel as="aside" className="work-inspector" aria-label={title} data-kind={side.type} data-expanded={expanded}
    drawer={compact} drawerClass="work-inspector-drawer" label={title} open onClose={close}
    edge="left" resizeKey={`inspector-${side.type}`} resizeLabel={title} onKeyDown={(event) => {
    if (!compact && event.key === "Escape" && !event.defaultPrevented) {
      event.stopPropagation();
      close();
    }
  }}>
    {tabs}
  </WorkbenchPanel>;
}
