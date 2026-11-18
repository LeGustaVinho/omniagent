import { useQuery, useQueryClient } from "@tanstack/react-query";
import { isSupportedWorkflowNode, type WorkflowDefinition, type WorkflowDefinitions, type WorkflowGraph, type WorkflowGraphNode } from "@/workflow-graph-model";
import { publishedWorkflowArtifactQuery, type PublishedWorkflowArtifact, type ReusableBody, type ReusableReference, type ReusableVersion } from "@/workflow-reusable";

export type WorkflowNodeAppearance = {
  type: "ai" | "no-ai" | "function" | "prefab" | "loop" | "reusable" | "unknown";
  usesAI?: boolean;
};

export type WorkflowNodeSource = {
  body: Pick<ReusableBody, "kind" | "graph" | "prompt">;
  references?: Record<string, ReusableReference>;
  definitions?: WorkflowDefinitions;
};

const modelExecutors = new Set(["conversation_agent", "plan_agent", "read_only_agent", "write_agent", "compare_models"]);

export function workflowNodeUsesAI(kind: string, definition?: WorkflowDefinition): boolean | undefined {
  if (definition?.kind === kind) return definition.effect === "invoke_model" || modelExecutors.has(definition.executor);
  if (isSupportedWorkflowNode(kind)) return kind === "agent";
}

export function nativeWorkflowNodeAppearance(kind: string, definition?: WorkflowDefinition): WorkflowNodeAppearance {
  if (kind === "subflow") return { type: "reusable" };
  if (kind === "loop") return { type: "loop" };
  const usesAI = workflowNodeUsesAI(kind, definition);
  return { type: usesAI === undefined ? "unknown" : usesAI ? "ai" : "no-ai", usesAI };
}

export function reusableWorkflowNodeAppearance(body: WorkflowNodeSource["body"] | undefined, usesAI?: boolean): WorkflowNodeAppearance {
  if (body?.kind === "subflow") return { type: "function", usesAI };
  if (body?.kind === "prompt" && body.prompt) {
    if (body.prompt.nodePrefab) return { type: "prefab", usesAI };
    if (usesAI !== undefined) return { type: usesAI ? "ai" : "no-ai", usesAI };
  }
  return { type: "reusable", usesAI };
}

export const workflowNodeVersionKey = ({ kind, destination, id, revision }: ReusableVersion) =>
  JSON.stringify([kind, destination, id, revision]);

function childReferences(graph?: WorkflowGraph, references?: Record<string, ReusableReference>) {
  return graph?.nodes.flatMap(({ id, kind }) => {
    const reference = (kind === "subflow" || kind === "loop") && references && Object.hasOwn(references, id) && references[id];
    return reference ? [reference] : [];
  }) ?? [];
}

export function publishedWorkflowNodeVersions(graph: WorkflowGraph, references: Record<string, ReusableReference>,
  drafts: ReadonlyMap<string, WorkflowNodeSource>) {
  const pending = childReferences(graph, references);
  const visitedDrafts = new Set<string>();
  const versions = new Map<string, ReusableVersion>();
  for (const reference of pending) {
    if (reference.source === "published" && reference.version.kind === "subflow") {
      versions.set(workflowNodeVersionKey(reference.version), reference.version);
    }
    if (reference.source === "draft" && !visitedDrafts.has(reference.key)) {
      visitedDrafts.add(reference.key);
      const draft = drafts.get(reference.key);
      pending.push(...childReferences(draft?.body.graph, draft?.references));
    }
  }
  return [...versions.values()];
}

const emptyArtifacts: ReadonlyMap<string, PublishedWorkflowArtifact> = new Map();

export function useWorkflowNodeArtifacts(projectId: string, versions: readonly ReusableVersion[], includeChildren = false) {
  const client = useQueryClient();
  const uniqueVersions = new Map(versions.map((version) => [workflowNodeVersionKey(version), version]));
  const keys = [...uniqueVersions.keys()].sort();
  return useQuery({
    queryKey: ["workflow-node-artifacts", projectId, includeChildren, keys],
    enabled: keys.length > 0,
    staleTime: 30_000,
    queryFn: async ({ signal }) => {
      const artifacts = new Map<string, PublishedWorkflowArtifact>();
      const visited = new Set<string>();
      let pending = uniqueVersions;
      while (pending.size) {
        signal.throwIfAborted();
        const batch = [...pending.entries()];
        pending = new Map();
        for (const [key] of batch) visited.add(key);
        const loaded = await Promise.all(batch.map(([, version]) => client.fetchQuery({
          ...publishedWorkflowArtifactQuery(projectId, version), staleTime: 30_000,
        }).catch(() => null)));
        batch.forEach(([key], index) => {
          const artifact = loaded[index];
          if (!artifact) return;
          artifacts.set(key, artifact);
          if (!includeChildren) return;
          for (const ref of childReferences(artifact.body.graph, artifact.references)) {
            if (ref.source !== "published" || ref.version.kind !== "subflow") continue;
            const childKey = workflowNodeVersionKey(ref.version);
            if (!visited.has(childKey)) pending.set(childKey, ref.version);
          }
        });
      }
      return artifacts;
    },
  }).data ?? emptyArtifacts;
}

export function resolveWorkflowNodeTypes(published: ReadonlyMap<string, WorkflowNodeSource>,
  drafts?: ReadonlyMap<string, WorkflowNodeSource>, catalog: readonly WorkflowDefinition[] = []) {
  const catalogByKind = new Map(catalog.map((definition) => [definition.kind, definition]));
  const visiting = new Set<WorkflowNodeSource>();
  const known = new Map<WorkflowNodeSource, boolean>();
  const sourceOf = (ref?: ReusableReference) => ref?.source === "draft" ? drafts?.get(ref.key)
    : ref?.source === "published" ? published.get(workflowNodeVersionKey(ref.version)) : undefined;

  function usesModel(source?: WorkflowNodeSource): boolean | undefined {
    if (!source || visiting.has(source)) return undefined;
    if (source.body.kind === "prompt") return source.body.prompt && workflowNodeUsesAI(source.body.prompt.executor);
    if (!source.body.graph) return undefined;
    if (known.has(source)) return known.get(source);
    visiting.add(source);
    const usage = source.body.graph.nodes.map(({ id, kind }) => kind === "loop" || kind === "subflow"
      ? usesModel(sourceOf(source.references?.[id]))
      : workflowNodeUsesAI(kind, source.definitions?.[id] ?? catalogByKind.get(kind)));
    visiting.delete(source);
    const result = usage.includes(true) ? true : usage.includes(undefined) ? undefined : false;
    if (result !== undefined) known.set(source, result);
    return result;
  }

  return {
    artifact: (source?: WorkflowNodeSource) => reusableWorkflowNodeAppearance(source?.body, usesModel(source)),
    node: (node: WorkflowGraphNode, definition?: WorkflowDefinition, reference?: ReusableReference): WorkflowNodeAppearance => {
      if (node.kind !== "subflow" && node.kind !== "loop") return nativeWorkflowNodeAppearance(node.kind, definition);
      const source = sourceOf(reference);
      const usesAI = usesModel(source);
      return node.kind === "loop" ? { type: "loop", usesAI } : reusableWorkflowNodeAppearance(source?.body, usesAI);
    },
  };
}
