const comparisonNotePrefix = "compare-note:";

export type WorkflowComparisonNote = { checkoutId: string; revisionId: string; relativePath: string; sha256: string };

export function workflowComparisonNoteTarget({ checkoutId, revisionId, relativePath, sha256 }: WorkflowComparisonNote) {
  return comparisonNotePrefix + encodeURIComponent(JSON.stringify({ checkoutId, revisionId, relativePath, sha256 }));
}

export function readWorkflowComparisonNoteTarget(target?: string): WorkflowComparisonNote | undefined {
  if (!target?.startsWith(comparisonNotePrefix)) return;
  try {
    const source: unknown = JSON.parse(decodeURIComponent(target.slice(comparisonNotePrefix.length)));
    if (!source || typeof source !== "object") return;
    const { checkoutId, revisionId, relativePath, sha256 } = source as Record<string, unknown>;
    if (typeof checkoutId === "string" && checkoutId.trim() && typeof revisionId === "string" && revisionId.trim()
      && typeof relativePath === "string" && relativePath.trim() && typeof sha256 === "string" && sha256.trim()) {
      return { checkoutId, revisionId, relativePath, sha256 };
    }
  } catch { /* Ignore malformed targets. */ }
}
