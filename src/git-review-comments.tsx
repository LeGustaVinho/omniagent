import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { MessageSquare } from "lucide-react";
import { useMemo, useRef, useState } from "react";
import type { useCheckoutConversationActions } from "@/checkout-conversation-actions";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import { excerptReference } from "@/editor-context";
import type { GitFileVersion } from "@/git-file-versions";
import { FileDialog } from "@/ide-file-workspace";
import { InlineIssue } from "@/tasks-git-ui";

type Revision = { branch: string | null; head: string | null; index: string | null };
type BranchRevisions = { base: string; head: string; mergeBase: string };
type Surface = "working" | "staged" | "branch" | "versions";
export type GitReviewSide = "original" | "modified";

type ScopeSource =
  | { kind: "checkout"; source: "working" | "staged"; revision: Revision }
  | { kind: "branch"; range: { baseReference: string; headReference: string | null }; revisions: BranchRevisions }
  | { kind: "versions"; base: GitFileVersion | null; revised: GitFileVersion; revision: Revision | null };
type Side = { path: string; sha256: string | null; lineCount: number };
export type GitReviewScope = {
  snapshot: string;
  source: ScopeSource;
  original: Side;
  modified: Side;
};

type Lines = { side: GitReviewSide; startLine: number; endLine: number };
export type GitReviewComment = Lines & {
  id: string;
  path: string;
  createdAt: number;
  resolved: boolean;
  scope: GitReviewScope;
  excerpt: string;
  body: string;
};

export type GitReviewAnnotations = {
  scope?: GitReviewScope;
  comments: readonly GitReviewComment[];
  location?: Lines & { snapshot: string };
  add: (lines: Lines) => void;
  show: (id: string) => void;
};

const sideLabels = { original: "Base", modified: "Revised" } as const;
const noComments: readonly GitReviewComment[] = [];

function sourceLabel(source: ScopeSource) {
  const revision = (entry: Revision) => `HEAD ${entry.head ?? "unborn"} · Index ${entry.index ?? "missing"}${entry.branch ? ` · ${entry.branch}` : ""}`;
  const version = (entry: GitFileVersion | null) => !entry ? "Missing" : entry.kind === "commit" ? entry.sha : entry.kind;
  switch (source.kind) {
    case "checkout": return `${source.source === "staged" ? "HEAD → index" : "Index → worktree"} · ${revision(source.revision)}`;
    case "branch": return `Merge base ${source.revisions.mergeBase} → ${source.revisions.head} · Base ${source.range.baseReference} ${source.revisions.base}`;
    case "versions": return `${version(source.base)} → ${version(source.revised)}${source.revision ? ` · ${revision(source.revision)}` : ""}`;
  }
}

function lineReference(scope: GitReviewScope, lines: Lines) {
  return `${excerptReference({ path: scope[lines.side].path, startLine: lines.startLine, endLine: lines.endLine })} · ${sideLabels[lines.side]}`;
}

function commentReference(comment: GitReviewComment) {
  return `${lineReference(comment.scope, comment)} · ${sourceLabel(comment.scope.source)} · SHA-256 ${comment.scope[comment.side].sha256} · Review comment ${comment.id}`;
}

type CommentDraft = Lines & { path: string; scope: GitReviewScope };
type StartContextText = ReturnType<typeof useCheckoutConversationActions>["startContextText"];

export function useGitReviewComments({ checkoutId, surface, matchPath, comparisonPath, scope, language, startContextText, enabled = true }: {
  checkoutId: string;
  surface: Surface;
  matchPath?: string;
  comparisonPath?: string;
  scope?: GitReviewScope | null;
  language?: string;
  startContextText?: StartContextText;
  enabled?: boolean;
}) {
  const client = useQueryClient();
  const details = useRef<HTMLDetailsElement>(null);
  const [draft, setDraft] = useState<CommentDraft>();
  const [body, setBody] = useState("");
  const [location, setLocation] = useState<GitReviewAnnotations["location"]>();
  const { copy, feedback } = useContextActionFeedback();
  const key = ["git-review-comments", checkoutId, surface];
  const query = useQuery({
    queryKey: [...key, matchPath ?? null],
    queryFn: () => invoke<GitReviewComment[]>("git_review_comments", { checkoutId, surface, path: matchPath ?? null }),
    enabled,
    retry: false,
  });
  const comments = query.data ?? noComments;
  const activeComments = useMemo(() => scope ? comments.filter((comment) => comment.scope.snapshot === scope.snapshot && !comment.resolved) : noComments, [comments, scope]);
  const save = useMutation({
    mutationFn: ({ path, scope, side, startLine, endLine, body }: CommentDraft & { body: string }) =>
      invoke<GitReviewComment>("git_add_review_comment", { checkoutId, request: { path, source: scope.source, snapshot: scope.snapshot, side, startLine, endLine, body } }),
    onSuccess: () => {
      setDraft(undefined);
      setBody("");
      if (details.current) details.current.open = true;
      return client.invalidateQueries({ queryKey: key });
    },
  });
  const resolve = useMutation({
    mutationFn: (comment: GitReviewComment) =>
      invoke<void>("git_set_review_comment_resolved", { checkoutId, commentId: comment.id, resolved: !comment.resolved }),
    onSuccess: () => client.invalidateQueries({ queryKey: key }),
  });
  function show(id: string) {
    if (!details.current) return;
    details.current.open = true;
    const comment = Array.from(details.current.querySelectorAll<HTMLElement>("[data-review-comment]")).find((item) => item.dataset.reviewComment === id);
    comment?.focus();
    comment?.scrollIntoView?.({ block: "nearest" });
  }
  function add(lines: Lines) {
    if (!scope || !comparisonPath || lines.startLine > scope[lines.side].lineCount) return;
    save.reset();
    setBody("");
    setDraft({ ...lines, endLine: Math.min(lines.endLine, scope[lines.side].lineCount), path: comparisonPath, scope });
  }
  const annotations: GitReviewAnnotations = {
    scope: scope ?? undefined,
    comments: activeComments,
    location: scope?.snapshot === location?.snapshot ? location : undefined,
    add,
    show,
  };
  const stale = draft && draft.scope.snapshot !== scope?.snapshot;
  const dialog = draft && <FileDialog title="Add review comment" description={lineReference(draft.scope, draft)}
    onClose={() => { if (!save.isPending) setDraft(undefined); }}>
    <form className="grid gap-2" onSubmit={(event) => {
      event.preventDefault();
      if (!stale && body.trim() && !save.isPending) save.mutate({ ...draft, body });
    }}>
      <label className="file-field">Comment
        <textarea autoFocus rows={5} maxLength={16_384} value={body} disabled={save.isPending}
          onChange={(event) => setBody(event.target.value)} />
      </label>
      {stale && <p role="alert">The comparison changed. Copy your comment and reselect the lines in the updated comparison.</p>}
      {save.error && <InlineIssue>{String(save.error)}</InlineIssue>}
      <div className="file-dialog-footer">
        <button type="button" className="fw-btn" disabled={save.isPending} onClick={() => setDraft(undefined)}>Cancel</button>
        <button type="submit" className="fw-btn fw-primary" disabled={Boolean(stale) || save.isPending || !body.trim()}>Save comment</button>
      </div>
    </form>
  </FileDialog>;
  const panel = <details ref={details} className="git-review-comments border-b border-border text-xs">
    <summary className="flex cursor-pointer items-center gap-2 px-3 py-2">
      <MessageSquare size={14} aria-hidden /> Review comments{comments.length > 0 && ` (${comments.length})`}
    </summary>
    <div className="max-h-56 overflow-auto px-3 pb-2">
      {query.isPending && <p role="status">Loading review comments…</p>}
      {(query.error || resolve.error) && <InlineIssue>{String(query.error || resolve.error)}</InlineIssue>}
      {query.isSuccess && !comments.length && <p>Right-click a line or selected range in the diff to leave a comment.</p>}
      {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
      <ol className="grid gap-2">
        {comments.map((comment) => {
          const current = comment.scope.snapshot === scope?.snapshot;
          const jump = () => setLocation({ side: comment.side, startLine: comment.startLine, endLine: comment.endLine, snapshot: comment.scope.snapshot });
          return <li key={comment.id} tabIndex={-1} data-review-comment={comment.id} className="rounded border border-border p-2">
            <ContextActions label={`review comment on ${lineReference(comment.scope, comment)}`} groups={[[
              ...(current ? [{ label: "Go to reviewed lines", run: jump }] : []),
              { label: "Copy comment", run: () => copy(comment.body) },
              { label: "Copy reviewed text", run: () => copy(comment.excerpt) },
              { label: "Copy review reference", run: () => copy(commentReference(comment)) },
              ...(startContextText ? [{ label: "Add review comment to conversation…", run: () => startContextText({
                title: `Git review · ${comment.scope[comment.side].path}`, reference: commentReference(comment),
                text: comment.excerpt, language: current ? language : undefined,
              }, comment.body) }] : []),
            ], [
              { label: comment.resolved ? "Reopen comment" : "Resolve comment", disabled: resolve.isPending, run: () => resolve.mutate(comment) },
            ]]}>
              <div className="min-w-0 flex-1">
                <p className="wrap-anywhere font-mono">{lineReference(comment.scope, comment)}</p>
                <p className="text-muted-foreground">{current ? "Current comparison" : "Saved snapshot"}{comment.resolved && " · Resolved"}</p>
                <p className="my-1 whitespace-pre-wrap wrap-anywhere">{comment.body}</p>
                <details>
                  <summary className="cursor-pointer">Reviewed text and revision</summary>
                  <p className="mt-1 wrap-anywhere font-mono">{sourceLabel(comment.scope.source)}</p>
                  <pre className="max-h-32 overflow-auto whitespace-pre-wrap wrap-anywhere">{comment.excerpt}</pre>
                </details>
                {current && <Button type="button" variant="ghost" className="text-xs" onClick={jump}>Go to reviewed lines</Button>}
              </div>
            </ContextActions>
          </li>;
        })}
      </ol>
    </div>
  </details>;
  return { annotations, panel, dialog };
}
