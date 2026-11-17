import {
  Activity, Blocks, BookOpen, ClipboardList, Code2, FlaskConical, GitBranch, ListChecks, MessageSquare,
  PanelLeft, RotateCcw, Settings, type LucideIcon,
} from "lucide-react";
import { Dialog, Tooltip } from "radix-ui";
import { lazy, Suspense, useEffect, useRef, useState, type ComponentType, type ReactNode } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { CheckoutSelector } from "@/checkout-selector";
import { Button } from "@/components/ui/button";
import { ConversationMarkdown } from "@/conversation-markdown";
import { previewPlan } from "@/conversation-preview-data";
import { RealConversations, RealDiagnostics } from "@/desktop-dock";
import { GitSimulationProvider } from "@/git-simulation";
import { openWorkspaceFile } from "@/file-context-actions";
import type { FileLinkTarget } from "@/file-links";
import { RunInspector, SimulationDiagnostics } from "@/inspection-panel";
import { InspectorTabs } from "@/inspector-tabs";
import { PanelResizeScope, ResizablePanel } from "@/panel-resize";
import { useRunSubscription } from "@/run-projection";
import { RunInspectorNavigation } from "@/run-inspector-navigation";
import {
  exampleCheckout, nextDeliveryStep, scenarios,
  type DeliveryAction, type DeliveryStep, type QuestionFocus, type QueueJobFocus, type ScenarioId, type SimulationSession,
  type WorkspaceDestination, type WorkspaceId, type WorkspaceProps,
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

type Navigation = { active: WorkspaceId; visited: Partial<Record<WorkspaceId, { target?: string }>> };
type SimulationState = { scenario: ScenarioId; epoch: number; delivery: DeliveryStep; navigation: Navigation };
type SidePane = { type: "activity"; runId: string; conversationId?: string; focusQuestion?: QuestionFocus }
  | { type: "changes"; target: "saved" | "working" };

function show(workspace: WorkspaceId, target?: string, prior?: Navigation): Navigation {
  return { active: workspace, visited: { ...prior?.visited, [workspace]: target === undefined ? prior?.visited[workspace] ?? {} : { target } } };
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

function useCompact() {
  const query = "(max-width: 959px)";
  const [compact, setCompact] = useState(() => window.matchMedia?.(query).matches ?? false);
  useEffect(() => {
    const media = window.matchMedia?.(query);
    if (!media) return;
    const update = () => setCompact(media.matches);
    media.addEventListener("change", update);
    return () => media.removeEventListener("change", update);
  }, []);
  return compact;
}

const loading = <p className="work-progress" role="status">Loading workspace…</p>;

function RailButton({ label, icon: Icon, current, pressed, onClick }: {
  label: string;
  icon: LucideIcon;
  current?: boolean;
  pressed?: boolean;
  onClick: () => void;
}) {
  return <Tooltip.Root>
    <Tooltip.Trigger asChild>
      <button type="button" className="work-rail-button" aria-label={label} aria-current={current ? "page" : undefined}
        aria-pressed={pressed} onClick={onClick}><Icon size={18} aria-hidden="true" /></button>
    </Tooltip.Trigger>
    <Tooltip.Portal><Tooltip.Content className="work-tooltip" side="right" sideOffset={8}>{label}</Tooltip.Content></Tooltip.Portal>
  </Tooltip.Root>;
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
  const [realNavigation, setRealNavigation] = useState(() => {
    const prior = readNavigation(lastAreaKey);
    if (destination) return show(destination.workspace, destination.target, prior);
    return initialConversationId ? show("conversations", initialConversationId, prior) : prior;
  });
  const [preview, setPreview] = useState<SimulationState | null>(null);
  const [epoch, setEpoch] = useState(0);
  const [settingsPage, setSettingsPage] = useState<string>();
  const [settingsVisited, setSettingsVisited] = useState(false);
  const [realSide, setRealSide] = useState<SidePane | null>(null);
  const [previewSide, setPreviewSide] = useState<SidePane | null>(null);
  const [realQueue, setRealQueue] = useState<"open" | "closed">();
  const [queueFocus, setQueueFocus] = useState<QueueJobFocus>();
  const [previewQueue, setPreviewQueue] = useState<"open" | "closed">();
  const [sidebarOverride, setSidebarOverride] = useState<boolean>();
  const sideTrigger = useRef<HTMLElement | null>(null);
  const sidebarTrigger = useRef<HTMLButtonElement>(null);
  const compact = useCompact();
  const sidebar = sidebarOverride ?? !compact;
  const { error, reconnect } = useRunSubscription(checkout);
  const nav = preview?.navigation ?? realNavigation;

  useEffect(() => {
    if (!fileTarget) return;
    setPreview(null);
    setSettingsPage(undefined);
    openWorkspaceFile((workspace, target) => setRealNavigation((prior) => show(workspace, target, prior)),
      fileTarget.path, fileTarget.line ?? undefined, { column: fileTarget.column ?? undefined, repeat: true });
  }, [fileTarget]);

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
        if (simulation) {
          setPreview((current) => current && ({ ...current, navigation: show(workspace, target, current.navigation) }));
        } else {
          setRealNavigation((current) => show(workspace, target, current));
        }
      },
      openActivity: (runId, conversationId, requestId) => showSide({
        type: "activity", runId, conversationId, focusQuestion: requestId === undefined ? undefined : { requestId },
      }),
      openChanges: (target) => showSide({ type: "changes", target }),
      openQueueJob: simulation ? undefined : (jobId) => {
        setQueueFocus({ projectId: checkout.projectId, jobId });
        setRealQueue("open");
      },
    };
  }

  const real = context(null);
  const simulated = preview && context({ scenario: preview.scenario, epoch: preview.epoch, delivery: preview.delivery, advanceDelivery });
  const current = simulated ?? real;
  const side = preview ? previewSide : realSide;
  function closeSide() {
    (preview ? setPreviewSide : setRealSide)(null);
    const trigger = sideTrigger.current;
    requestAnimationFrame(() => {
      if (trigger?.getClientRects().length) trigger.focus();
      else document.querySelector<HTMLElement>('.work-rail-button[aria-current="page"]')?.focus();
    });
  }

  function changeSidebar(visible: boolean) {
    setSidebarOverride(visible);
    if (!visible && compact) requestAnimationFrame(() => sidebarTrigger.current?.focus());
  }
  const queue = (preview ? previewQueue : realQueue) === "open";
  const changeQueue = preview ? setPreviewQueue : setRealQueue;
  const currentArea = areas.find(({ id }) => id === nav.active)?.label ?? "Diagnostics";

  return (
    <div className="work-shell flex min-h-0 flex-1 flex-col">
      <header className="work-titlebar">
        <div className="work-wordmark"><span className="work-mark" aria-hidden="true"><Blocks size={15} /></span><span className="work-brand-name">OmniAgent</span></div>
        <div className="work-breadcrumb">
          {nav.active === "conversations" && !preview && <Button ref={sidebarTrigger} variant="ghost" className="p-1.5" aria-label="Toggle conversation navigation" aria-expanded={sidebar}
            onClick={() => changeSidebar(!sidebar)} title="Toggle conversation navigation"><PanelLeft size={17} /></Button>}
          {preview ? <span className="truncate font-medium">Example repository</span> : <CheckoutSelector checkout={checkout}
            onSelect={async (selected, shortcut) => {
              if (shortcut && selected.checkoutId === checkout.checkoutId) real.openWorkspace(shortcut.workspace, shortcut.target);
              else await onSelectCheckout(selected, undefined, shortcut);
            }} />}
          <span className="work-crumb-divider" aria-hidden="true">/</span><span className="work-current-area">{currentArea}</span>
        </div>
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
      <div className="work-body">
        <aside aria-label="Workspace" className="work-rail">
          <Tooltip.Provider delayDuration={350} skipDelayDuration={200}>
          <nav aria-label="Workspaces" className="grid gap-1">
            {areas.map(({ id, icon, label }) => <RailButton key={id} label={label} icon={icon} current={nav.active === id}
              onClick={() => current.openWorkspace(id)} />)}
          </nav>
          <div className="work-rail-end">
            <RailButton label="Queue" icon={ListChecks} pressed={queue} onClick={() => changeQueue(queue ? "closed" : "open")} />
            <RailButton label="Diagnostics" icon={Activity} current={nav.active === "diagnostics"} onClick={() => current.openWorkspace("diagnostics")} />
            <RailButton label="Settings" icon={Settings} onClick={() => showSettings("appearance")} />
          </div>
          </Tooltip.Provider>
        </aside>
        <div className="work-decks">
          <div className="work-deck" hidden={Boolean(preview)}>
            <RunInspectorNavigation.Provider value={{ openActivity: real.openActivity, openWorkspace: real.openWorkspace, activeRunId: realSide?.type === "activity" ? realSide.runId : undefined,
              openGitAttempt: (runId) => real.openWorkspace("git", `attempt:${runId}`) }}>
              <WorkspaceViews nav={realNavigation} context={real} sidebar={!preview && sidebar} onSidebarChange={changeSidebar} compact={compact}
                onOpenOtherCheckout={onSelectCheckout} openSystem={() => showSettings("system")} />
            </RunInspectorNavigation.Provider>
          </div>
          {preview && simulated && <PanelResizeScope scope="simulation" key={"simulation-workspaces:" + preview.epoch}>
            <div className="work-deck">
              <WorkspaceViews nav={preview.navigation} context={simulated} sidebar={sidebar} onSidebarChange={changeSidebar}
                compact={compact} openSystem={() => showSettings("system")} />
            </div>
          </PanelResizeScope>}
          {realQueue && <ResizablePanel className="work-queue-panel" edge="top" resizeKey="queue" resizeLabel="queue" hidden={Boolean(preview) || realQueue !== "open"}>
            <Suspense fallback={loading}><QueuePanel {...real} focusJob={queueFocus}
              onOpenRegisteredWorktree={(source, destination) => onSelectCheckout(source, undefined, destination)}
              active={!preview && realQueue === "open"} onClose={() => setRealQueue("closed")} /></Suspense>
          </ResizablePanel>}
          {previewQueue && simulated && <ResizablePanel className="work-queue-panel" edge="top" resizeKey="example-queue" resizeLabel="queue" key={"simulation-queue:" + preview?.epoch} hidden={previewQueue !== "open"}>
            <PanelResizeScope scope="simulation">
              <Suspense fallback={loading}><QueuePanel {...simulated} onClose={() => setPreviewQueue("closed")} /></Suspense>
            </PanelResizeScope>
          </ResizablePanel>}
        </div>
        {side && <PanelResizeScope scope={preview ? "simulation" : null}>
          <SideInspector key={side.type === "activity" ? side.runId : side.target} side={side} compact={compact} close={closeSide}
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
      </GitSimulationProvider>
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
  onOpenOtherCheckout?: (checkout: ProjectCheckout, conversationId: string) => Promise<void>;
}) {
  return Object.entries(nav.visited).map(([id, state]) => {
    const workspace = id as WorkspaceId;
    const View = workspaceComponents[workspace];
    const visible = nav.active === workspace;
    const target = state?.target;
    return <div key={id} className="work-view" hidden={!visible}>
      <Suspense fallback={loading}>
        {workspace === "conversations" ? context.simulation
          ? <ConversationsPreview {...context} target={target} compact={compact} />
          : <RealConversations checkout={context.checkout} target={target} sidebar={visible && sidebar} onSidebarChange={onSidebarChange}
            compact={compact} onOpenOtherCheckout={onOpenOtherCheckout} />
          : workspace === "diagnostics" ? context.simulation
            ? <SimulationDiagnostics {...context} openSystem={openSystem} />
            : <RealDiagnostics checkout={context.checkout} openSystem={openSystem} />
            : View && <View {...context} target={target} />}
      </Suspense>
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
  if (compact) return (
    <Dialog.Root open onOpenChange={(open) => { if (!open) close(); }}>
      <Dialog.Portal>
        <Dialog.Overlay className="work-shade" />
        <Dialog.Content asChild aria-describedby={undefined}>
          <ResizablePanel className="work-inspector work-inspector-drawer" data-kind={side.type} data-expanded={expanded}
            edge="left" resizeKey={`inspector-${side.type}`} resizeLabel={title}>
            <Dialog.Title className="sr-only">{title}</Dialog.Title>
            {tabs}
          </ResizablePanel>
        </Dialog.Content>
      </Dialog.Portal>
    </Dialog.Root>
  );
  return <ResizablePanel as="aside" className="work-inspector" aria-label={title} data-kind={side.type} data-expanded={expanded}
    edge="left" resizeKey={`inspector-${side.type}`} resizeLabel={title} onKeyDown={(event) => {
    if (event.key === "Escape" && !event.defaultPrevented) {
      event.stopPropagation();
      close();
    }
  }}>
    {tabs}
  </ResizablePanel>;
}
