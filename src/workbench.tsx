import {
  Activity, ArrowLeft, Blocks, BookOpen, ClipboardList, Code2, FlaskConical, GitBranch, ListChecks, MessageSquare,
  PanelLeft, PanelLeftClose, PanelLeftOpen, RotateCcw, Search, Settings, type LucideIcon,
} from "lucide-react";
import { lazy, Suspense, useEffect, useRef, useState, type ComponentType, type ReactNode } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { CheckoutSelector } from "@/checkout-selector";
import { Button } from "@/components/ui/button";
import { ControlTooltip } from "@/components/ui/control-tooltip";
import { ConversationMarkdown } from "@/conversation-markdown";
import { previewPlan } from "@/conversation-preview-data";
import { RealConversations, RealDiagnostics } from "@/desktop-dock";
import { GitSimulationProvider } from "@/git-simulation";
import { openWorkspaceFile } from "@/file-context-actions";
import type { FileLinkTarget } from "@/file-links";
import { RunInspector, SimulationDiagnostics } from "@/inspection-panel";
import { InspectorTabs } from "@/inspector-tabs";
import { PanelResizeScope } from "@/panel-resize";
import { useRunSubscription } from "@/run-projection";
import { RunInspectorNavigation } from "@/run-inspector-navigation";
import { GoTo, type Destination } from "@/workbench-navigation";
import { useWorkspaceSize, WorkbenchPanel, WorkspaceDrawersSuspended, WorkspaceVisibility } from "@/workbench-panel";
import {
  exampleCheckout, nextDeliveryStep, scenarios,
  type DeliveryAction, type DeliveryStep, type QuestionFocus, type QueueJobFocus, type ScenarioId, type SimulationSession, type WorkspaceId, type WorkspaceProps,
} from "@/workbench-contract";

const ConversationsPreview = lazy(() => import("@/conversations-preview"));
const SettingsDialog = lazy(() => import("@/settings-dialog"));
const QueuePanel = lazy(() => import("@/queue-panel"));
const ChangesPanel = lazy(() => import("@/changes-panel"));
const workspaceComponents: Partial<Record<WorkspaceId, ComponentType<WorkspaceProps>>> = {
  ide: lazy(() => import("@/ide-workspace")),
  tasks: lazy(() => import("@/tasks-workspace")),
  pipelines: lazy(() => import("@/pipelines-workspace")),
  knowledge: lazy(() => import("@/knowledge-workspace")),
  git: lazy(() => import("@/git-workspace")),
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
type SimulationState = { scenario: ScenarioId; epoch: number; delivery: DeliveryStep; navigation: Navigation };
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

export default function Workbench({ checkout, openRepository, onSelectCheckout, initialConversationId, fileTarget }: {
  checkout: ProjectCheckout;
  openRepository: ReactNode;
  onSelectCheckout: (checkout: ProjectCheckout, conversationId?: string) => Promise<void>;
  initialConversationId?: string;
  fileTarget?: FileLinkTarget;
}) {
  const lastAreaKey = "omniagent-workspace-area:" + checkout.checkoutId;
  const [realNavigation, setRealNavigation] = useState(() => {
    const prior = readNavigation(lastAreaKey);
    return initialConversationId ? show("conversations", initialConversationId, prior) : prior;
  });
  const [preview, setPreview] = useState<SimulationState | null>(null);
  const [epoch, setEpoch] = useState(0);
  const [settingsPage, setSettingsPage] = useState<string>();
  const [settingsVisited, setSettingsVisited] = useState(false);
  const [realSide, setRealSide] = useState<SidePane | null>(null);
  const [previewSide, setPreviewSide] = useState<SidePane | null>(null);
  const [closingInspector, setClosingInspector] = useState(false);
  const [realQueue, setRealQueue] = useState<"open" | "closed">();
  const [queueFocus, setQueueFocus] = useState<QueueJobFocus>();
  const [previewQueue, setPreviewQueue] = useState<"open" | "closed">();
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
  const nav = preview?.navigation ?? realNavigation;

  useEffect(() => {
    if (!fileTarget) return;
    setPreview(null);
    setSettingsPage(undefined);
    openWorkspaceFile((workspace, target) => setRealNavigation((prior) => show(workspace, target, prior)),
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
      localStorage.setItem(lastAreaKey, realNavigation.active);
    } catch {
      // Navigation remains available in memory.
    }
  }, [lastAreaKey, realNavigation.active]);

  function startSimulation(choice?: ScenarioId) {
    const scenario = choice ?? (nav.active === "pipelines" ? "pipelines" : "delivery");
    const example = scenarios[scenario];
    const destination = choice ? example.workspace : nav.active;
    const target = choice ? example.target : nav.visited[destination]?.target;
    setEpoch(epoch + 1);
    setPreview({ scenario, epoch: epoch + 1, delivery: example.initialStep, navigation: show(destination, target) });
    setPreviewSide(null);
    setPreviewQueue(undefined);
    if (scenario === "windows") showSettings("system");
    else setSettingsPage(undefined);
  }

  function leaveSimulation() {
    setSettingsPage(undefined);
    setPreviewSide(null);
    setPreviewQueue(undefined);
    setPreview(null);
  }

  function advanceDelivery(action: DeliveryAction) {
    setPreview((current) => current && ({ ...current, delivery: nextDeliveryStep(current.delivery, action) }));
  }

  function changeNavigation(simulated: boolean, update: (prior: Navigation) => Navigation) {
    if (simulated) {
      setPreview((current) => current && ({ ...current, navigation: update(current.navigation) }));
    } else {
      setRealNavigation(update);
    }
  }

  function navigateArea(destination: WorkspaceId) {
    changeNavigation(Boolean(preview), (prior) => show(destination, undefined, prior, []));
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
    const view = workViews.current?.querySelector('.work-deck:not([hidden]) .work-view:not([hidden])');
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
    changeNavigation(Boolean(preview), (prior) => ({
      ...prior,
      active: origin.area,
      visited: { ...prior.visited, [origin.area]: { target: origin.target } },
      trail: prior.trail.slice(0, -1),
    }));
    (preview ? setPreviewSide : setRealSide)(origin.inspector);
    if (origin.queue) (preview ? setPreviewQueue : setRealQueue)("open");
    requestAnimationFrame(() => {
      for (const { element, top, left } of origin.scroll) {
        if (element.isConnected) { element.scrollTop = top; element.scrollLeft = left; }
      }
      restoreWorkspaceFocus(origin.trigger);
    });
  }

  function context(simulation: SimulationSession | null): WorkspaceProps {
    const setSide = simulation ? setPreviewSide : setRealSide;
    function showSide(next: SidePane) {
      const active = document.activeElement;
      if (active instanceof HTMLElement && !active.closest(".work-inspector")) sideTrigger.current = active;
      const existing = simulation ? previewSide : realSide;
      if (next.type === "activity" && !next.focusQuestion && existing?.type === "activity" && next.runId === existing.runId) {
        requestAnimationFrame(() => document.querySelector<HTMLElement>('.work-inspector [aria-label="Close activity"]')?.focus());
        return;
      }
      setSide(next);
    }
    return {
      checkout: simulation ? exampleCheckout : checkout,
      simulation,
      startSimulation,
      openWorkspace: (workspace, target) => {
        const source = simulation ? preview?.navigation : realNavigation;
        const inspector = simulation ? previewSide : realSide;
        const focused = document.activeElement;
        const trigger = focused instanceof HTMLElement && focused !== document.body ? focused : null;
        const jumping = source && source.active !== workspace;
        const fromQueue = Boolean(trigger?.closest(".work-queue-panel"));
        const scroll = jumping ? scrollPositions(workViews.current?.querySelector<HTMLElement>(".work-deck:not([hidden]) .work-view:not([hidden])"), trigger) : [];
        if (jumping && inspector) setSide(null);
        if (jumping && fromQueue && queueDrawer) (simulation ? setPreviewQueue : setRealQueue)("closed");
        changeNavigation(Boolean(simulation), (prior) => show(workspace, target, prior, jumping ? [...prior.trail, {
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
      openChanges: (target) => showSide({ type: "changes", target }),
      openQueueJob: simulation ? undefined : (jobId) => {
        rememberQueueTrigger();
        if (queueDrawer) setRealSide(null);
        setQueueFocus({ projectId: checkout.projectId, jobId });
        setRealQueue("open");
      },
    };
  }

  const real = context(null);
  const simulated = preview && context({ scenario: preview.scenario, epoch: preview.epoch, delivery: preview.delivery, advanceDelivery });
  const current = simulated ?? real;
  const side = preview ? previewSide : realSide;
  const sideDrawer = availableWidth < (side?.type === "changes" ? 990 : 840);
  const sidebar = sidebarOverride ?? !compact;
  function closeSide() {
    setClosingInspector(true);
    (preview ? setPreviewSide : setRealSide)(null);
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
  const queue = (preview ? previewQueue : realQueue) === "open";
  const changeQueue = preview ? setPreviewQueue : setRealQueue;
  function rememberQueueTrigger() {
    const active = document.activeElement;
    if (active instanceof HTMLElement && !active.closest(".work-queue-panel")) queueTrigger.current = active;
  }
  function showQueue() {
    rememberQueueTrigger();
    if (queueDrawer) {
      (preview ? setPreviewSide : setRealSide)(null);
    }
    changeQueue("open");
  }
  function closeQueue() {
    changeQueue("closed");
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
          {nav.active === "conversations" && !preview && <ControlTooltip label="Toggle conversation navigation">
            <Button ref={sidebarTrigger} variant="ghost" className="ui-icon-button" aria-label="Toggle conversation navigation" aria-expanded={sidebar}
              onClick={() => changeSidebar(!sidebar)}><PanelLeft size={16} aria-hidden="true" /></Button>
          </ControlTooltip>}
          {preview ? <span className="truncate font-medium">Example repository</span> : <CheckoutSelector checkout={checkout} onSelect={onSelectCheckout} />}
          <span className="work-crumb-divider" aria-hidden="true">/</span><span className="work-current-area">{currentArea}</span>
        </div>
        {!narrowRail && navigationSearch}
        {!preview && <div className="flex shrink-0 items-center gap-2">
          <Button variant="ghost" className="px-2.5" onClick={() => startSimulation()} title="Try a workflow with example data"><FlaskConical size={15} />Simulate</Button>
          {openRepository}
        </div>}
      </header>
      {preview && <div className="work-simulation" role="region" aria-label="Simulation controls">
        <span className="work-simulation-label"><FlaskConical size={15} />Simulation · example data</span>
        <label className="flex min-w-0 items-center gap-2">
          <span className="sr-only">Scenario</span>
          <select value={preview.scenario} onChange={(event) => startSimulation(event.target.value as ScenarioId)}>
            {Object.entries(scenarios).map(([id, entry]) => <option key={id} value={id}>{entry.name}</option>)}
          </select>
        </label>
        <Button variant="ghost" className="work-reset ml-auto" aria-label="Reset simulation" onClick={() => startSimulation(preview.scenario)}><RotateCcw size={14} /><span>Reset simulation</span></Button>
        <Button variant="outline" onClick={leaveSimulation}>Exit simulation</Button>
      </div>}
      {!preview && error && (
        <div className="work-banner" role="alert"><span>{error}</span><Button variant="outline" onClick={reconnect}>Reconnect</Button></div>
      )}
      <GitSimulationProvider epoch={preview?.epoch}>
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
          <div className="work-deck" hidden={Boolean(preview)}>
            <RunInspectorNavigation.Provider value={{ openActivity: real.openActivity, openWorkspace: real.openWorkspace, activeRunId: realSide?.type === "activity" ? realSide.runId : undefined,
              openGitAttempt: (runId) => real.openWorkspace("git", `attempt:${runId}`), openProviders: () => showSettings("providers") }}>
              <WorkspaceViews nav={realNavigation} context={real} sidebar={!preview && sidebar && !(compact && (side || queueDrawer && queue))} onSidebarChange={changeSidebar} compact={compact}
                deckVisible={!preview} onOpenOtherCheckout={onSelectCheckout} openSystem={() => showSettings("system")} />
            </RunInspectorNavigation.Provider>
          </div>
          {preview && simulated && <PanelResizeScope scope="simulation" key={"simulation-workspaces:" + preview.epoch}>
            <div className="work-deck">
              <WorkspaceViews nav={preview.navigation} context={simulated} sidebar={sidebar} onSidebarChange={changeSidebar}
                compact={compact} openSystem={() => showSettings("system")} />
            </div>
          </PanelResizeScope>}
          </WorkspaceDrawersSuspended.Provider>
          {realQueue && <WorkbenchPanel className="work-queue-panel" edge="top" resizeKey="queue" resizeLabel="queue"
            drawer={queueDrawer} drawerClass="work-queue-drawer" label="Queue" open={!preview && realQueue === "open" && !(queueDrawer && side)} onClose={closeQueue}>
            <Suspense fallback={loading}><QueuePanel {...real} focusJob={queueFocus}
              active={!preview && realQueue === "open"} onClose={closeQueue} /></Suspense>
          </WorkbenchPanel>}
          {previewQueue && simulated && <WorkbenchPanel className="work-queue-panel" edge="top" resizeKey="example-queue" resizeLabel="queue"
            key={"simulation-queue:" + preview?.epoch} drawer={queueDrawer} drawerClass="work-queue-drawer" label="Queue"
            open={previewQueue === "open" && !(queueDrawer && side)} onClose={closeQueue}>
            <PanelResizeScope scope="simulation">
              <Suspense fallback={loading}><QueuePanel {...simulated} onClose={closeQueue} /></Suspense>
            </PanelResizeScope>
          </WorkbenchPanel>}
        </div>
        {side && <PanelResizeScope scope={preview ? "simulation" : null}>
          <SideInspector key={side.type === "activity" ? side.runId : side.target} side={side} compact={sideDrawer} close={closeSide}
            plan={simulated && side.type === "changes" && side.target === "saved" && <SavedPlan />}>
            <Suspense fallback={loading}>
              {side.type === "activity" ? <RunInspector {...current} runId={side.runId} conversationId={side.conversationId}
                focusQuestion={side.focusQuestion} onClose={closeSide} /> : <PanelResizeScope scope="changes-inspector">
                <ChangesPanel {...current} target={side.target} />
              </PanelResizeScope>}
            </Suspense>
          </SideInspector>
        </PanelResizeScope>}
      </div>
      {settingsVisited && <PanelResizeScope scope={preview ? "simulation" : null}>
        <Suspense fallback={null}><SettingsDialog key={preview?.epoch ?? "real"} {...current} open={Boolean(settingsPage)} initialPage={settingsPage}
          onOpenChange={(open) => { if (!open) setSettingsPage(undefined); }} /></Suspense>
      </PanelResizeScope>}
      {goTo && <GoTo destinations={destinations} onClose={closeGoTo} />}
      </GitSimulationProvider>
    </div>
  );
}

function WorkspaceViews({ nav, context, sidebar, onSidebarChange, compact, deckVisible = true, openSystem, onOpenOtherCheckout }: {
  nav: Navigation;
  context: WorkspaceProps;
  sidebar: boolean;
  onSidebarChange: (visible: boolean) => void;
  compact: boolean;
  deckVisible?: boolean;
  openSystem?: () => void;
  onOpenOtherCheckout?: (checkout: ProjectCheckout, conversationId: string) => Promise<void>;
}) {
  return Object.entries(nav.visited).map(([id, state]) => {
    const workspace = id as WorkspaceId;
    const View = workspaceComponents[workspace];
    const visible = nav.active === workspace;
    const target = state?.target;
    return <div key={id} className="work-view" hidden={!visible}>
      <WorkspaceVisibility.Provider value={deckVisible && visible}><Suspense fallback={loading}>
        {workspace === "conversations" ? context.simulation
          ? <ConversationsPreview {...context} target={target} compact={compact} />
          : <RealConversations checkout={context.checkout} target={target} sidebar={visible && sidebar} onSidebarChange={onSidebarChange}
            compact={compact} onOpenOtherCheckout={onOpenOtherCheckout} />
          : workspace === "diagnostics" ? context.simulation
            ? <SimulationDiagnostics {...context} openSystem={openSystem} />
            : <RealDiagnostics checkout={context.checkout} openSystem={openSystem} />
            : View && <View {...context} target={target} />}
      </Suspense></WorkspaceVisibility.Provider>
    </div>;
  });
}

function SavedPlan() {
  return <article className="work-inspection-body max-h-full overflow-auto">
    <p className="font-mono text-xs">Plan revision 1 · docs/release.md</p>
    <ConversationMarkdown>{previewPlan}</ConversationMarkdown>
  </article>;
}

function SideInspector({ side, compact, close, children, plan }: { side: SidePane; compact: boolean; close: () => void; children: ReactNode; plan?: ReactNode }) {
  const [expanded, setExpanded] = useState(false);
  const title = side.type === "changes" ? "Changes" : "Activity";
  const tabs = <InspectorTabs title={title} content={children} plan={plan} expanded={expanded} onExpand={() => setExpanded(!expanded)} onClose={close} />;
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
