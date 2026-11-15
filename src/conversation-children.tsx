export type ChildActivity = {
  type: "child_activity";
  child_id: string | null;
  status: string | null;
  details?: {
    parentId: string | null;
    prompt: string | null;
    requestedModel: string | null;
    reportedModel: string | null;
  };
};

export type ChildLocation = {
  sessionId: string | null;
  turnId: string | null;
  itemId: string | null;
  parentItemId: string | null;
};

type ChildEntry = {
  sessionId: string;
  parentId: string | null;
  childId: string;
  status: string | null;
  prompt: string | null;
  requestedModel: string | null;
  reportedModel: string | null;
};

export type Delegation = {
  children: Record<string, ChildEntry>;
  parentActivity: Record<string, string>;
};

export function recordChildActivity(delegation: Delegation, activity: ChildActivity, location: ChildLocation) {
  const { details } = activity;
  const sessionId = location.sessionId;
  const childId = activity.child_id;
  const explicitParent = details?.parentId ?? location.parentItemId;
  const related = Object.entries(delegation.children).filter(([, child]) =>
    child.sessionId === sessionId && child.childId === childId);
  if (!sessionId || !childId?.trim() || (!explicitParent && related.length > 1)) {
    const key = JSON.stringify([sessionId, location.turnId, location.itemId ?? activity.status]);
    delegation.parentActivity[key] = activity.status ?? "Status unavailable";
    return;
  }

  const single = related.length === 1 ? related[0] : undefined;
  const parentId = explicitParent ?? single?.[1].parentId ?? null;
  const key = JSON.stringify([sessionId, parentId, childId]);
  let previous = delegation.children[key];
  if (!previous && single && single[1].parentId === null) {
    previous = single[1];
    delete delegation.children[single[0]];
  }
  delegation.children[key] = {
    sessionId, parentId, childId,
    status: activity.status ?? previous?.status ?? null,
    prompt: details?.prompt ?? previous?.prompt ?? null,
    requestedModel: details?.requestedModel ?? previous?.requestedModel ?? null,
    reportedModel: details?.reportedModel ?? previous?.reportedModel ?? null,
  };
}

export function ConversationChildren({ delegation }: { delegation: Delegation }) {
  const children = Object.entries(delegation.children);
  const activity = Object.entries(delegation.parentActivity);
  if (!children.length && !activity.length) return null;
  return (
    <section aria-label="Delegation" className="grid gap-2">
      <h2 className="text-sm font-medium">Delegation</h2>
      {children.map(([key, child]) => (
        <details key={key} className="rounded-md border border-border bg-card p-3">
          <summary className="cursor-pointer break-all text-sm">Child {child.childId} · {child.status ?? "Status unavailable"}</summary>
          <dl className="mt-2 grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 break-words text-xs text-muted-foreground">
            <dt>Provider session</dt><dd>{child.sessionId}</dd>
            <dt>Parent</dt><dd>{child.parentId ?? "Unavailable"}</dd>
            <dt>Prompt</dt><dd className="whitespace-pre-wrap">{child.prompt ?? "Unavailable"}</dd>
            <dt>Context</dt><dd>Unavailable</dd>
            <dt>{child.reportedModel ? "Model" : child.requestedModel ? "Requested model" : "Model"}</dt>
            <dd>{child.reportedModel ?? child.requestedModel ?? "Unavailable"}</dd>
            <dt>Controls</dt><dd>Unavailable</dd>
          </dl>
        </details>
      ))}
      {activity.map(([key, status]) => <p key={key} className="text-xs text-muted-foreground">Parent delegation · {status}</p>)}
    </section>
  );
}
