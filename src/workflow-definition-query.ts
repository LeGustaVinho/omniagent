import { invoke } from "@tauri-apps/api/core";
import { keepPreviousData, queryOptions } from "@tanstack/react-query";
import { isSupportedWorkflowNode, type WorkflowDefinition } from "@/workflow-graph-model";

export function workflowNodeDefinitionsQuery(nodeKinds: readonly string[]) {
  const kinds = [...new Set(nodeKinds.filter(isSupportedWorkflowNode))].sort();
  return queryOptions({
    queryKey: ["workflow-node-definitions", kinds],
    staleTime: Infinity,
    initialData: kinds.length ? undefined : [],
    placeholderData: keepPreviousData,
    queryFn: () => invoke<WorkflowDefinition[]>("workflow_node_definitions", { kinds }),
  });
}
