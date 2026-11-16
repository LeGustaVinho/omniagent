import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useId, useState } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { Button } from "@/components/ui/button";

type Field = "live_question_timeout" | "template_display_label";
type Grant = { field: Field; checkoutId: string | null; templateId: string | null };
type Allowlist = { revision: number; grants: Grant[] };
type Setting<T> = { revision: number; value: T };
type Diff =
  | { field: "live_question_timeout"; before: Setting<number>; after: number }
  | { field: "template_display_label"; templateId: string; templateRevision: string; before: Setting<string | null>; after: string | null };
type AppliedRevision = { field: Field; templateId: string | null; beforeRevision: number; afterRevision: number };
type RecordedDecision = { source: "user_allowlist" | "desktop_user"; allowlistRevision: number; at: number };
type Decision =
  | { status: "pending" }
  | ({ status: "applied"; revisions: AppliedRevision[] } & RecordedDecision)
  | ({ status: "rejected" } & RecordedDecision);
type Proposal = {
  id: string;
  actor: { projectId: string; checkoutId: string; runId: string; turnId: string; dispatchId: string };
  requestId: string;
  reviewRevision: string;
  proposedAt: number;
  allowlistRevision: number;
  diffs: Diff[];
  decision: Decision;
};
type Template = { templateId: string; name: string };

const fieldNames: Record<Field, string> = {
  live_question_timeout: "Live question timeout",
  template_display_label: "Template display label",
};
const decisionNames: Record<Decision["status"], string> = {
  pending: "Awaiting review",
  applied: "Applied",
  rejected: "Rejected",
};
const pageSize = 20;
const maxGrants = 16;
const selectStyle = "w-full rounded-md border border-input bg-background px-2 py-1.5 text-sm";

function checkoutName(path: string) {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
}

function grantKey(grant: Grant) {
  return JSON.stringify([grant.field, grant.checkoutId, grant.templateId]);
}

function DateStamp({ seconds }: { seconds: number }) {
  const date = new Date(seconds * 1000);
  return <time dateTime={date.toISOString()}>{date.toLocaleString()}</time>;
}

function ProposalDiff({ diff, templateName, applied }: { diff: Diff; templateName?: string; applied?: AppliedRevision }) {
  const display = (value: string | number | null) => value === null ? "Default template name"
    : diff.field === "live_question_timeout" ? `${value} min` : value;
  return <li className="rounded-md border border-border p-3">
    <h4 className="font-medium">{fieldNames[diff.field]}{templateName && ` · ${templateName}`}</h4>
    {diff.field === "template_display_label" && <div className="mt-1 break-all text-xs text-muted-foreground">
      <div>Template ID <code>{diff.templateId}</code></div>
      <div>Template revision <code>{diff.templateRevision}</code></div>
    </div>}
    <dl className="mt-2 grid grid-cols-2 gap-3 break-words text-sm">
      <div><dt className="text-muted-foreground">Before · revision {diff.before.revision}</dt><dd>{display(diff.before.value)}</dd></div>
      <div><dt className="text-muted-foreground">{applied ? `Applied · revision ${applied.afterRevision}` : "Proposed"}</dt><dd>{display(diff.after)}</dd></div>
    </dl>
  </li>;
}

function ProposalReview({ projectId, templates, checkouts, onOpenRun }: {
  projectId: string; templates: ReadonlyMap<string, string>; checkouts: ReadonlyMap<string, string>; onOpenRun: (runId: string) => void;
}) {
  const client = useQueryClient();
  const title = useId();
  const [offset, setOffset] = useState(0);
  const key = ["workflow-settings-proposals", projectId];
  const proposals = useQuery({ queryKey: [...key, offset], queryFn: () => invoke<Proposal[]>("settings_proposals", { projectId, offset }) });
  const review = useMutation({
    mutationFn: ({ proposalId, expectedReviewRevision, accept }: { proposalId: string; expectedReviewRevision: string; accept: boolean }) =>
      invoke<Proposal>("review_settings_proposal", { projectId, proposalId, expectedReviewRevision, accept }),
    onSuccess: async (saved) => {
      client.setQueriesData<Proposal[]>({ queryKey: key }, (current) => current?.map((item) => item.id === saved.id ? saved : item));
      if (saved.decision.status !== "applied") return;
      await Promise.all([
        client.invalidateQueries({ queryKey: ["project-question-timeout", projectId] }),
        client.invalidateQueries({ queryKey: ["template-display-label", projectId] }),
        client.invalidateQueries({ queryKey: ["conversation-templates", projectId], exact: true }),
      ]);
    },
    onError: () => client.invalidateQueries({ queryKey: key }),
  });
  return <section className="space-y-3" data-setting="settings-proposals" aria-labelledby={title}>
    <div className="flex flex-wrap items-center justify-between gap-2">
      <h3 id={title} className="font-semibold">Settings and template proposals</h3>
      <Button type="button" variant="outline" disabled={proposals.isFetching} onClick={() => void proposals.refetch()}>Refresh proposals</Button>
    </div>
    {proposals.isPending && <p role="status">Loading proposals…</p>}
    {proposals.error && <p role="alert" className="settings-result settings-result-error">{String(proposals.error)}</p>}
    {proposals.data?.length === 0 && <p className="text-sm text-muted-foreground">{offset === 0 ? "No requests for this project." : "No more proposals."}</p>}
    <ul className="space-y-2" aria-label="Proposal history">
      {proposals.data?.map((proposal) => <li key={proposal.id}>
        <details className="rounded-md border border-border p-3">
          <summary className="cursor-pointer text-sm font-medium">
            {decisionNames[proposal.decision.status]} · {proposal.diffs.length} {proposal.diffs.length === 1 ? "field" : "fields"} · <DateStamp seconds={proposal.proposedAt} />
          </summary>
          <div className="mt-3 space-y-3">
            <ul className="space-y-2" aria-label="Proposed field changes">
              {proposal.diffs.map((diff) => {
                const templateId = diff.field === "template_display_label" ? diff.templateId : null;
                const applied = proposal.decision.status === "applied" ? proposal.decision.revisions.find((entry) =>
                  entry.field === diff.field && entry.templateId === templateId) : undefined;
                return <ProposalDiff key={`${diff.field}:${templateId ?? ""}`} diff={diff} templateName={templateId ? templates.get(templateId) : undefined} applied={applied} />;
              })}
            </ul>
            <dl className="space-y-1 break-all text-xs text-muted-foreground">
              <div><dt className="inline">Checkout </dt><dd className="inline">{checkouts.get(proposal.actor.checkoutId) ?? proposal.actor.checkoutId}</dd></div>
              <div><dt className="inline">Review revision </dt><dd className="inline"><code>{proposal.reviewRevision}</code></dd></div>
              <div><dt className="inline">Allowlist at proposal </dt><dd className="inline">Revision {proposal.allowlistRevision}</dd></div>
              <div><dt className="inline">Originating run </dt><dd className="inline"><code>{proposal.actor.runId}</code></dd></div>
              <div><dt className="inline">Turn </dt><dd className="inline"><code>{proposal.actor.turnId}</code></dd></div>
              <div><dt className="inline">Dispatch </dt><dd className="inline"><code>{proposal.actor.dispatchId}</code></dd></div>
            </dl>
            <div className="flex flex-wrap gap-2">
              <Button type="button" variant="outline" onClick={() => onOpenRun(proposal.actor.runId)}>Open originating run</Button>
              {proposal.decision.status === "pending" && <>
                <Button type="button" disabled={review.isPending} onClick={() => review.mutate({ proposalId: proposal.id, expectedReviewRevision: proposal.reviewRevision, accept: true })}>Apply all changes</Button>
                <Button type="button" variant="outline" disabled={review.isPending} onClick={() => review.mutate({ proposalId: proposal.id, expectedReviewRevision: proposal.reviewRevision, accept: false })}>Reject proposal</Button>
              </>}
            </div>
            {review.error && review.variables?.proposalId === proposal.id && <p role="alert" className="settings-result settings-result-error">{String(review.error)}</p>}
            {proposal.decision.status !== "pending" && <p className="text-sm text-muted-foreground">
              {decisionNames[proposal.decision.status]} by {proposal.decision.source === "user_allowlist" ? "user allowlist" : "desktop user"} · Allowlist revision {proposal.decision.allowlistRevision} · <DateStamp seconds={proposal.decision.at} />
            </p>}
          </div>
        </details>
      </li>)}
    </ul>
    {(offset > 0 || proposals.data?.length === pageSize) && <nav aria-label="Proposal pages" className="flex gap-2">
      <Button type="button" variant="outline" disabled={offset === 0} onClick={() => setOffset(offset - pageSize)}>Newer proposals</Button>
      <Button type="button" variant="outline" disabled={proposals.data?.length !== pageSize || offset + pageSize > 16_384} onClick={() => setOffset(offset + pageSize)}>Older proposals</Button>
    </nav>}
  </section>;
}

function AllowlistEditor({ checkout, checkouts, templates }: {
  checkout: ProjectCheckout; checkouts: readonly ProjectCheckout[]; templates: readonly Template[];
}) {
  const { projectId } = checkout;
  const client = useQueryClient();
  const title = useId();
  const key = ["workflow-settings-allowlist", projectId];
  const allowlist = useQuery({ queryKey: key, queryFn: () => invoke<Allowlist>("settings_patch_allowlist", { projectId }) });
  const [draft, setDraft] = useState<Allowlist>();
  const [field, setField] = useState<Field>("live_question_timeout");
  const [checkoutId, setCheckoutId] = useState(checkout.checkoutId);
  const [templateId, setTemplateId] = useState("");
  const addGrant: Grant = { field, checkoutId: checkoutId || null, templateId: field === "template_display_label" ? templateId || null : null };
  const visible = draft ?? allowlist.data;
  const stale = draft !== undefined && allowlist.data?.revision !== draft.revision;
  const save = useMutation({
    mutationFn: ({ revision: expectedRevision, grants }: Allowlist) => invoke<Allowlist>("set_settings_patch_allowlist", { projectId, expectedRevision, grants }),
    onSuccess: (saved) => { client.setQueryData(key, saved); setDraft(undefined); },
    onError: () => client.invalidateQueries({ queryKey: key }),
  });
  function edit(grants: Grant[]) {
    if (visible) setDraft({ ...visible, grants });
    save.reset();
  }
  const checkoutScope = (id: string | null) => id ? checkouts.find((item) => item.checkoutId === id)?.checkoutRoot ?? id : "All project checkouts";
  const templateScope = (id: string | null) => id ? templates.find((item) => item.templateId === id)?.name ?? id : "All project templates";
  return <section className="space-y-3 border-t border-border pt-4" data-setting="allowed-settings-fields" aria-labelledby={title}>
    <h3 id={title} className="font-semibold">Allowlisted fields</h3>
    <p className="text-sm text-muted-foreground">A proposal applies automatically when every changed field matches a project grant.</p>
    {allowlist.isPending && <p role="status">Loading allowlist…</p>}
    {allowlist.error && <p role="alert" className="settings-result settings-result-error">{String(allowlist.error)} <button type="button" onClick={() => void allowlist.refetch()}>Retry</button></p>}
    {visible && <>
      <p className="text-sm text-muted-foreground">Saved revision {allowlist.data?.revision ?? visible.revision}{draft && " · Unsaved changes"}</p>
      {visible.grants.length === 0 && <p className="text-sm">{draft ? "No automatic grants in this draft." : "All proposals require review."}</p>}
      <ul className="space-y-2" aria-label="Automatic grants">
        {visible.grants.map((grant) => <li key={grantKey(grant)} className="flex flex-wrap items-center justify-between gap-2 rounded-md border border-border p-2 text-sm">
          <div className="min-w-0 break-all">
            <div className="font-medium">{fieldNames[grant.field]}</div>
            <div className="text-muted-foreground">{checkoutScope(grant.checkoutId)}{grant.field === "template_display_label" && ` · ${templateScope(grant.templateId)}`}</div>
          </div>
          <Button type="button" variant="outline" disabled={save.isPending || stale} aria-label={`Remove grant for ${fieldNames[grant.field]} in ${checkoutScope(grant.checkoutId)}${grant.field === "template_display_label" ? ` and ${templateScope(grant.templateId)}` : ""}`}
            onClick={() => edit(visible.grants.filter((entry) => grantKey(entry) !== grantKey(grant)))}>Remove</Button>
        </li>)}
      </ul>
      <fieldset className="grid gap-2 text-sm" disabled={save.isPending || stale}>
        <legend className="sr-only">Add automatic grant</legend>
        <label>Setting field<select className={selectStyle} value={field} onChange={(event) => {
          setField(event.target.value as Field);
          setTemplateId("");
        }}>{Object.entries(fieldNames).map(([id, name]) => <option key={id} value={id}>{name}</option>)}</select></label>
        <label>Checkout scope<select className={selectStyle} value={checkoutId} onChange={(event) => setCheckoutId(event.target.value)}>
          <option value="">All project checkouts</option>
          {checkouts.map((item) => <option key={item.checkoutId} value={item.checkoutId} title={item.checkoutRoot}>{checkoutName(item.checkoutRoot)}</option>)}
        </select></label>
        {field === "template_display_label" && <label>Template scope<select className={selectStyle} value={templateId} onChange={(event) => setTemplateId(event.target.value)}>
          <option value="">All project templates</option>
          {templates.map((item) => <option key={item.templateId} value={item.templateId}>{item.name}</option>)}
        </select></label>}
        <Button className="justify-self-start" type="button" variant="outline" disabled={visible.grants.length >= maxGrants || visible.grants.some((existing) => grantKey(existing) === grantKey(addGrant))}
          onClick={() => edit([...visible.grants, addGrant])}>Add grant</Button>
      </fieldset>
      <div className="flex flex-wrap gap-2">
        <Button type="button" disabled={!draft || stale || save.isPending} onClick={() => draft && save.mutate(draft)}>Save allowlist</Button>
        {draft && <Button type="button" variant="outline" disabled={save.isPending} onClick={() => { setDraft(undefined); save.reset(); }}>
          {stale ? "Load latest allowlist" : "Discard allowlist edits"}
        </Button>}
      </div>
      {stale && <p role="alert" className="settings-result settings-result-error">The saved allowlist changed. Load the latest revision before editing it.</p>}
      {save.error && <p role="alert" className="settings-result settings-result-error">{String(save.error)}</p>}
      {save.isSuccess && <p role="status" className="settings-result settings-result-success">Allowlist saved.</p>}
    </>}
  </section>;
}

export function WorkflowSettingsProposals({ checkout, onOpenRun }: { checkout: ProjectCheckout; onOpenRun: (runId: string) => void }) {
  const { projectId } = checkout;
  const checkouts = useQuery({ queryKey: ["registered-checkouts"], queryFn: () => invoke<ProjectCheckout[]>("registered_checkouts") });
  const templates = useQuery({ queryKey: ["conversation-templates", projectId], queryFn: () => invoke<Template[]>("list_conversation_templates", { projectId }) });
  const scopedCheckouts = (checkouts.data ?? []).filter((item) => item.projectId === projectId);
  if (!scopedCheckouts.some((item) => item.checkoutId === checkout.checkoutId)) scopedCheckouts.push(checkout);
  return <div className="space-y-5">
    <ProposalReview projectId={projectId} checkouts={new Map(scopedCheckouts.map((item) => [item.checkoutId, item.checkoutRoot]))}
      templates={new Map((templates.data ?? []).map((item) => [item.templateId, item.name]))} onOpenRun={onOpenRun} />
    <AllowlistEditor key={checkout.checkoutId} checkout={checkout} checkouts={scopedCheckouts} templates={templates.data ?? []} />
    {(checkouts.error || templates.error) && <p role="alert" className="settings-result settings-result-error">{String(checkouts.error || templates.error)}</p>}
  </div>;
}
