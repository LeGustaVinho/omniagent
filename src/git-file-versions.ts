import { invoke } from "@tauri-apps/api/core";
import type { GitReviewScope } from "@/git-review-comments";

export type GitFileSide = { status: "missing" } | { status: "text"; content: string } | { status: "unavailable"; reason: string };
export type GitFileVersion = { kind: "head" | "index" | "worktree" } | { kind: "commit"; sha: string; path: string };
export type GitPermalinkLines = { startLine: number; endLine: number; text: string };

export function compareGitFileVersions(checkoutId: string, path: string, base: GitFileVersion | null, revised: GitFileVersion) {
  return invoke<{ original: GitFileSide; modified: GitFileSide; commentScope?: GitReviewScope | null }>("git_compare_file_versions", { checkoutId, path, base, revised });
}

export function gitFileText(side: GitFileSide) {
  if (side.status === "unavailable") throw new Error(side.reason);
  return side.status === "text" ? side.content : "";
}

export function gitFilePermalink(checkoutId: string, path: string, sha?: string, lines?: GitPermalinkLines) {
  return invoke<string>("git_file_permalink", { checkoutId, path, sha: sha ?? null, lines: lines ?? null });
}
