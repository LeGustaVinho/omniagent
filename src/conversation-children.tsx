import { useId, useMemo } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { Provider } from "@/bindings/Provider";
import type { QueueState } from "@/bindings/QueueState";
import type { RunStatus } from "@/bindings/RunStatus";
import { Button } from "@/components/ui/button";
import type { WorkspaceProps } from "@/workbench-contract";

export type ChildActivity = {
  type: "child_activity";
  child_id: string | null;
  status: string | null;
  details?: {
    parentId: string | null;
    prompt: string | null;
    requestedModel: string | null;
    reportedModel: string | null;
    contextProvenance?: string;
    response?: string;
  };
};

export type ChildLocation = {
  sessionId: string | null;
  turnId: string | null;
  itemId: string | null;
  parentItemId: string | null;
};

type ChildEntry = {
  scope: string;
  sessionId: string | null;
  parentId: string | null;
  parentKind: "agent" | "item" | null;
  childId: string;
  status: string | null;
  prompt: string | null;
  requestedModel: string | null;
  reportedModel: string | null;
  contextProvenance: string | null;
  response: string | null;
};

export type Delegation = {
  children: Record<string, ChildEntry>;
  parentActivity: Record<string, string>;
};

export function recordChildActivity(delegation: Delegation, activity: ChildActivity, location: ChildLocation, attemptId?: string) {
  const { details } = activity;
  const sessionId = location.sessionId;
  const scope = sessionId ? JSON.stringify(["provider", sessionId]) : attemptId ? JSON.stringify(["attempt", attemptId]) : null;
  const childId = activity.child_id;
  const reportedParent = details?.parentId?.trim() ? details.parentId : null;
  const explicitParent = reportedParent ?? location.parentItemId;
  const related = Object.entries(delegation.children).filter(([, child]) =>
    child.scope === scope && child.childId === childId);
  if (!scope || !childId?.trim() || activity.status === "notFound" || (!explicitParent && related.length > 1)) {
    const key = JSON.stringify([sessionId, location.turnId, location.itemId ?? activity.status]);
    delegation.parentActivity[key] = activity.status ?? "Status unavailable";
    return;
  }

  const single = related.length === 1 ? related[0] : undefined;
  const parentId = explicitParent ?? single?.[1].parentId ?? null;
  const key = JSON.stringify([scope, parentId, childId]);
  let previous = delegation.children[key];
  if (!previous && single && single[1].parentId === null) {
    previous = single[1];
    delete delegation.children[single[0]];
  }
  delegation.children[key] = {
    scope, sessionId, parentId, childId,
    parentKind: reportedParent || previous?.parentKind === "agent" ? "agent" : parentId ? "item" : null,
    status: activity.status ?? previous?.status ?? null,
    prompt: details?.prompt ?? previous?.prompt ?? null,
    requestedModel: details?.requestedModel ?? previous?.requestedModel ?? null,
    reportedModel: details?.reportedModel ?? previous?.reportedModel ?? null,
    contextProvenance: details?.contextProvenance ?? previous?.contextProvenance ?? null,
    response: details?.response ?? previous?.response ?? null,
  };
}

function nativeParentLinks(children: [string, ChildEntry][]) {
  const byIdentity = new Map<string, string[]>();
  for (const [key, child] of children) {
    const identity = JSON.stringify([child.scope, child.childId]);
    byIdentity.set(identity, [...(byIdentity.get(identity) ?? []), key]);
  }
  const parents = new Map<string, string>();
  for (const [key, child] of children) {
    const reported = byIdentity.get(JSON.stringify([child.scope, child.parentId]));
    if (child.parentKind === "agent" && reported?.length === 1 && reported[0] && reported[0] !== key) parents.set(key, reported[0]);
  }
  for (const key of parents.keys()) {
    const chain = new Set([key]);
    let parent = parents.get(key);
    while (parent && !chain.has(parent)) {
      chain.add(parent);
      parent = parents.get(parent);
    }
    if (parent) for (const member of chain) parents.delete(member);
  }
  return parents;
}

export function ConversationChildren({ delegation }: { delegation: Delegation }) {
  const anchor = useId();
  const children = Object.entries(delegation.children);
  const anchors = new Map(children.map(([key], index) => [key, `${anchor}-native-${index}`]));
  const parents = nativeParentLinks(children);
  const activity = Object.entries(delegation.parentActivity);
  if (!children.length && !activity.length) return null;

  const branch = (parent?: string) => children.filter(([key]) => parents.get(key) === parent).map(([key, child]) => {
    const descendants = branch(key);
    const parentAnchor = parent && anchors.get(parent);
    return <li key={key} id={anchors.get(key)} className="min-w-0 grid gap-2">
      <details className="rounded-md border border-border bg-card p-3">
        <summary className="cursor-pointer break-all text-sm">Native child {child.childId} · {child.status ?? "Status unreported"}</summary>
        <dl className="mt-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 break-words text-xs text-muted-foreground">
          <dt>Provider session</dt><dd>{child.sessionId ?? "Unreported"}</dd>
          <dt>{child.parentKind === "item" ? "Provider parent item" : "Provider parent"}</dt>
          <dd>{parentAnchor ? <a href={`#${parentAnchor}`} className="underline">{child.parentId}</a> : child.parentId ?? "Unreported"}</dd>
          <dt>Prompt</dt><dd className="whitespace-pre-wrap">{child.prompt ?? "Unreported"}</dd>
          <dt>Context provenance</dt><dd>{child.contextProvenance ?? "Unreported"}</dd>
          <dt>Reasoning</dt><dd>Unreported</dd>
          <dt>Requested model</dt><dd>{child.requestedModel ?? "Unreported"}</dd>
          <dt>Reported model</dt><dd>{child.reportedModel ?? "Unreported"}</dd>
          <dt>Reported response</dt><dd className="whitespace-pre-wrap">{child.response ?? "Unreported"}</dd>
          <dt>Controls</dt><dd>Unreported</dd>
        </dl>
      </details>
      {descendants.length > 0 && <ul aria-label="Native descendants" className="grid gap-2 border-l border-border pl-3">{descendants}</ul>}
    </li>;
  });
  return <section aria-label="Native delegation" className="grid gap-2">
    <ul className="grid gap-2">{branch()}</ul>
    {activity.map(([key, status]) => <p key={key} className="text-xs text-muted-foreground">Parent delegation · {status}</p>)}
  </section>;
}

type OpenActivity = WorkspaceProps["openActivity"];
type AttemptLink = { runId: string; conversationId: string };
type Attempt = AttemptLink & {
  ordinal: number;
  previousRunId: string | null;
  status: RunStatus;
  provider: Provider;
  model: string | null;
  reportedModel: string | null;
  promptDelivery: "prepared" | "submitted" | "unconfirmed" | null;
  contextRecorded: boolean;
};
type ManagedJob = {
  id: string;
  parent: AttemptLink;
  queueState: QueueState;
  queuedRecovery: boolean;
  attempts: Attempt[];
};
export type AgentTree = {
  parent: AttemptLink | null;
  attempts: Attempt[];
  managed: ManagedJob[];
  nativeObservations: { runId: string; event: { data: ChildActivity; location: ChildLocation } }[];
};

const statuses: Record<RunStatus, string> = {
  starting: "Starting", running: "Running", waiting_for_input: "Waiting for input", succeeded: "Completed",
  failed: "Failed", cancelled: "Cancelled", interrupted: "Interrupted",
};
const queues: Record<QueueState, string> = { queued: "Queued", paused: "Paused", cancelled: "Cancelled" };
const providers: Record<Provider, string> = { codex: "Codex", claude: "Claude Code", antigravity: "Antigravity" };
const promptDelivery = { prepared: "Prepared", submitted: "Submitted", unconfirmed: "Submission unconfirmed" };

function activeJob(job: ManagedJob) {
  const status = job.attempts.at(-1)?.status;
  return status === "starting" || status === "running" || status === "waiting_for_input" ||
    (job.queueState === "queued" && (!status || job.queuedRecovery));
}

function AttemptRow({ attempt, selected, previous, openActivity }: {
  attempt: Attempt;
  selected?: boolean;
  previous?: AttemptLink;
  openActivity?: OpenActivity;
}) {
  return <div className="grid gap-1">
    <p className="text-sm">Attempt {attempt.ordinal} · {providers[attempt.provider]} · {statuses[attempt.status]}{selected ? " · Current view" : ""}</p>
    <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 break-words text-xs text-muted-foreground">
      <dt>Requested model</dt><dd>{attempt.model ?? "Unreported"}</dd>
      <dt>Reported model</dt><dd>{attempt.reportedModel ?? "Unreported"}</dd>
      <dt>Prompt</dt><dd>{attempt.promptDelivery ? promptDelivery[attempt.promptDelivery] : "Unreported"}</dd>
      <dt>Context</dt><dd>{attempt.contextRecorded ? "Saved snapshot" : "Unreported"}</dd>
    </dl>
    {openActivity && <div className="flex flex-wrap gap-2">
      {!selected && <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs"
        onClick={() => openActivity(attempt.runId, attempt.conversationId)}>Open attempt {attempt.ordinal}</Button>}
      {previous && <Button type="button" variant="outline" className="px-2.5 py-1.5 text-xs"
        onClick={() => openActivity(previous.runId, previous.conversationId)}>Open previous attempt</Button>}
    </div>}
  </div>;
}

type TreeBranchProps = {
  managedByParent: ReadonlyMap<string, ManagedJob[]>;
  nativeByAttempt: ReadonlyMap<string, Delegation>;
  attemptsById: ReadonlyMap<string, AttemptLink>;
  openActivity?: OpenActivity;
};

function ManagedBranches({ parentId, ...tree }: TreeBranchProps & { parentId: string }) {
  const children = tree.managedByParent.get(parentId) ?? [];
  if (!children.length) return null;
  return <ul aria-label="Managed children" className="grid gap-2 border-l border-border pl-3">
    {children.map((job) => {
      const latest = job.attempts.at(-1);
      const status = job.queueState !== "queued" ? queues[job.queueState] : job.queuedRecovery ? "Retry queued" :
        latest ? statuses[latest.status] : queues.queued;
      return <li key={job.id} className="grid gap-2 rounded-md border border-border bg-card p-3">
        <p className="break-words text-sm font-medium">Managed child{latest ? ` · ${providers[latest.provider]}` : ""} · {status}</p>
        {tree.openActivity && <Button type="button" variant="outline" className="justify-self-start px-2.5 py-1.5 text-xs"
          onClick={() => tree.openActivity?.(job.parent.runId, job.parent.conversationId)}>Open parent attempt</Button>}
        {job.attempts.map((attempt) => {
          const native = tree.nativeByAttempt.get(attempt.runId);
          return <div key={attempt.runId} className="grid gap-2 border-t border-border pt-2">
            <AttemptRow attempt={attempt} openActivity={tree.openActivity}
              previous={attempt.previousRunId ? tree.attemptsById.get(attempt.previousRunId) : undefined} />
            {native && <ConversationChildren delegation={native} />}
            <ManagedBranches parentId={attempt.runId} {...tree} />
          </div>;
        })}
      </li>;
    })}
  </ul>;
}

export function ConversationAgentTree({ conversationId, runId, live, delegation, openActivity }: {
  conversationId: string;
  runId: string;
  live: boolean;
  delegation?: Delegation;
  openActivity?: OpenActivity;
}) {
  const saved = useQuery({
    queryKey: ["conversation-run", runId, "agent-tree", conversationId],
    queryFn: () => invoke<AgentTree>("conversation_agent_tree", { conversationId, runId }),
    refetchInterval: ({ state }) => live || state.data?.managed.some(activeJob) ? 1000 : false,
  });
  const tree = useMemo(() => {
    const managedByParent = new Map<string, ManagedJob[]>();
    const nativeByAttempt = new Map<string, Delegation>();
    const attemptsById = new Map<string, AttemptLink>();
    for (const attempt of saved.data?.attempts ?? []) attemptsById.set(attempt.runId, attempt);
    for (const job of saved.data?.managed ?? []) {
      managedByParent.set(job.parent.runId, [...(managedByParent.get(job.parent.runId) ?? []), job]);
      for (const attempt of job.attempts) attemptsById.set(attempt.runId, attempt);
    }
    for (const { runId: owner, event } of saved.data?.nativeObservations ?? []) {
      const native = nativeByAttempt.get(owner) ?? { children: {}, parentActivity: {} };
      recordChildActivity(native, event.data, event.location, owner);
      nativeByAttempt.set(owner, native);
    }
    return { managedByParent, nativeByAttempt, attemptsById };
  }, [saved.data]);
  const rootNative = saved.data ? tree.nativeByAttempt.get(runId) :
    delegation && (Object.keys(delegation.children).length || Object.keys(delegation.parentActivity).length) ? delegation : undefined;
  const history = saved.data?.attempts ?? [];
  const parent = saved.data?.parent;
  if (!rootNative && !parent && !saved.data?.managed.length && history.length < 2 && !saved.error) return null;
  return <section aria-label="Agent tree" className="grid gap-2">
    <h2 className="text-sm font-medium">Agent tree</h2>
    {saved.error && <p role="alert" className="text-xs text-muted-foreground">{String(saved.error)}</p>}
    {parent && openActivity && <Button type="button" variant="outline" className="justify-self-start px-2.5 py-1.5 text-xs"
      onClick={() => openActivity(parent.runId, parent.conversationId)}>Open parent attempt</Button>}
    {history.length > 1 && <details className="rounded-md border border-border bg-card p-3">
      <summary className="cursor-pointer text-sm">Attempt history</summary>
      <ul className="mt-2 grid gap-3">{[...history].reverse().map((attempt) => <li key={attempt.runId}>
        <AttemptRow attempt={attempt} selected={attempt.runId === runId} openActivity={openActivity}
          previous={attempt.previousRunId ? tree.attemptsById.get(attempt.previousRunId) : undefined} />
      </li>)}</ul>
    </details>}
    {rootNative && <ConversationChildren delegation={rootNative} />}
    <ManagedBranches parentId={runId} {...tree} openActivity={openActivity} />
  </section>;
}
