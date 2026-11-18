import { ProjectTasksWorkspace } from "@/project-tasks";
import type { WorkspaceProps } from "@/workbench-contract";

export default function TasksWorkspace(props: WorkspaceProps) {
  return <ProjectTasksWorkspace key={props.checkout.projectId} {...props} />;
}
