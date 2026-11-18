import { Button } from "@/components/ui/button";
import type { WorkspaceProps } from "@/workbench-contract";
import { PipelineAuthoringEditor, ProjectPipelineDrafts } from "@/workflow-authoring";
import { WorkflowMaintenanceCandidates, WorkflowMaintenancePlugins } from "@/workflow-maintenance";
import { WorkflowSettingsProposals } from "@/workflow-settings-proposals";
import { WorkflowExecutionWorkspace } from "@/workflow-execution";
import { libraryCategories, WorkflowLibrary, type LibraryCategory } from "@/workflow-library";
import { type WorkflowExecutionFocus } from "@/workflow-execution-model";
import { WorkflowComparisons } from "@/workflow-comparison";
import { WorkflowTriggers } from "@/workflow-triggers";
import { PipelineMaintenanceLayout, PipelineTabs, PipelineWorkspaceFrame } from "@/pipeline-layout";
import { useWorkflowDraftNavigation, WorkflowDraftNavigation } from "@/workflow-draft-navigation";

const pipelineTabs = [
  { id: "drafts", label: "Builder" },
  { id: "runs", label: "Runs" },
  { id: "library", label: "Library" },
  { id: "compare", label: "Compare models" },
  { id: "automations", label: "Automation rules" },
  { id: "maintenance", label: "Maintenance" },
] as const;
type PipelineView = (typeof pipelineTabs)[number]["id"];
type LibraryView = `library:${LibraryCategory}`;

function LibraryNavigation({ active, onSelect }: { active: LibraryView; onSelect: (view: LibraryView) => void }) {
  return <nav className="workflow-library-navigation" aria-label="Library views">
    {libraryCategories.map(({ id, label }) => <Button key={id} type="button" variant="ghost"
      aria-current={active === `library:${id}` ? "page" : undefined}
      onClick={() => onSelect(`library:${id}`)}>{label}</Button>)}
  </nav>;
}

const isRolesTarget = (target?: string) => target === "library:roles" || target === "templates" || target === "roles";

function pipelineRoute(target = ""): { view: PipelineView; libraryView?: LibraryView; draftId?: string; runId?: string; runFocus?: WorkflowExecutionFocus } {
  if (target.startsWith("run:")) {
    const [runId, search] = target.slice("run:".length).split("?", 2);
    const params = new URLSearchParams(search);
    const nodeId = params.get("node");
    return { view: "runs", runId, runFocus: nodeId ? { nodeId, artifactId: params.get("artifact") ?? undefined } : undefined };
  }
  if (target.startsWith("authoring:")) return { view: "drafts", draftId: target.slice("authoring:".length) };
  if (target.startsWith("compare-note:")) return { view: "compare" };
  if (isRolesTarget(target)) return { view: "library", libraryView: "library:roles" };
  const libraryView = libraryCategories.find(({ id }) => target === `library:${id}`);
  if (libraryView) return { view: "library", libraryView: `library:${libraryView.id}` };
  const tab = pipelineTabs.find(({ id }) => id === target);
  if (tab) return { view: tab.id };
  if (!target || target === "builder" || target === "delivery") return { view: "drafts" };
  return { view: "runs", runId: target };
}

function ProjectPipelineMaintenance({ checkout, openActivity, openWorkspace }: Pick<WorkspaceProps, "checkout" | "openActivity" | "openWorkspace">) {
  return <PipelineMaintenanceLayout proposals={<WorkflowSettingsProposals checkout={checkout} onOpenRun={openActivity} compact />}
    releases={<WorkflowMaintenanceCandidates checkout={checkout} openWorkspace={openWorkspace} />}
    inventory={<WorkflowMaintenancePlugins />} />;
}

export default function PipelinesWorkspace(props: WorkspaceProps) {
  return <WorkflowDraftNavigation key={props.checkout.checkoutId}><ProjectPipelinesWorkspace {...props} /></WorkflowDraftNavigation>;
}

function ProjectPipelinesWorkspace({ openWorkspace, ...workspace }: WorkspaceProps) {
  const navigate = useWorkflowDraftNavigation();
  const props: WorkspaceProps = { ...workspace, openWorkspace: (area, target) => {
    if (area === "pipelines" && target !== workspace.target) navigate(() => openWorkspace(area, target));
    else openWorkspace(area, target);
  } };
  const { view, libraryView = "library:pipeline", draftId, runId, runFocus } = pipelineRoute(props.target);
  const category = libraryView.slice("library:".length) as LibraryCategory;
  return <PipelineWorkspaceFrame>
    <PipelineTabs views={pipelineTabs} active={view} onSelect={(tab) => {
      if (tab !== "library" || view !== "library") props.openWorkspace("pipelines", tab);
    }} />
    <div className="pipeline-view">
      {view === "compare" ? <WorkflowComparisons key={props.target ?? "compare"} {...props} />
        : view === "automations" ? <WorkflowTriggers key={props.checkout.checkoutId} {...props} />
        : view === "runs" ? <WorkflowExecutionWorkspace {...props} runId={runId} runFocus={runFocus} />
        : view === "library" ? <>
          <LibraryNavigation active={libraryView} onSelect={(target) => {
            if (target !== libraryView) props.openWorkspace("pipelines", target);
          }} />
          <WorkflowLibrary key={libraryView} category={category} {...props} />
        </>
        : view === "maintenance" ? <ProjectPipelineMaintenance {...props} />
        : draftId ? <PipelineAuthoringEditor key={draftId} {...props} draftId={draftId} />
          : <ProjectPipelineDrafts {...props} />}
    </div>
  </PipelineWorkspaceFrame>;
}
