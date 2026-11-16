import { useId } from "react";
import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import type { Provider } from "@/bindings/Provider";
import type { RunStatus } from "@/bindings/RunStatus";
import type { SessionMode } from "@/bindings/SessionMode";
import { Button } from "@/components/ui/button";
import type { WorkspaceProps } from "@/workbench-contract";

export type TaskParticipantAttempt = {
  runId: string;
  conversationId: string;
  ordinal: number;
  role: { kind: "workflow"; value: string } | { kind: "session"; value: SessionMode };
  provider: Provider;
  profileId: string;
  requestedModel: string | null;
  reportedModel: string | null;
  status: RunStatus;
};

const providerNames: Record<Provider, string> = { codex: "Codex", claude: "Claude Code", antigravity: "Antigravity" };
const sessionNames: Record<SessionMode, string> = { read_only: "Read-only", plan: "Planning", approved_write: "Implementation" };
const statusNames: Record<RunStatus, string> = {
  starting: "Starting", running: "Running", waiting_for_input: "Waiting for input", succeeded: "Succeeded",
  failed: "Failed", cancelled: "Cancelled", interrupted: "Interrupted",
};

function isActive(status: RunStatus) {
  return status === "starting" || status === "running" || status === "waiting_for_input";
}

type OpenActivity = WorkspaceProps["openActivity"];

function ParticipantAttempt({ attempt, openActivity }: { attempt: TaskParticipantAttempt; openActivity: OpenActivity }) {
  const role = attempt.role.kind === "workflow" ? attempt.role.value : sessionNames[attempt.role.value];
  return <li className="pipeline-evidence grid gap-2 break-words">
    <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
      <p className="font-medium">{role} · {providerNames[attempt.provider]}</p>
      <span className="text-sm text-muted-foreground">{statusNames[attempt.status]}</span>
    </div>
    <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-2 gap-y-1 text-sm">
      <dt className="text-muted-foreground">Profile</dt><dd>{attempt.profileId}</dd>
      <dt className="text-muted-foreground">Requested model</dt><dd>{attempt.requestedModel ?? "Profile default"}</dd>
      <dt className="text-muted-foreground">Reported model</dt><dd>{attempt.reportedModel ?? "Unreported"}</dd>
    </dl>
    <Button className="justify-self-start px-2.5 py-1.5 text-xs" type="button" variant="outline"
      onClick={() => openActivity(attempt.runId, attempt.conversationId)}>Open attempt {attempt.ordinal}</Button>
  </li>;
}

export function TaskParticipants({ projectId, taskId, openActivity }: {
  projectId: string;
  taskId: string;
  openActivity: OpenActivity;
}) {
  const heading = useId();
  const workingHeading = useId();
  const historyHeading = useId();
  const attempts = useQuery({
    queryKey: ["project-tasks", projectId, taskId, "participants"],
    queryFn: () => invoke<TaskParticipantAttempt[]>("project_task_runs", { projectId, taskId }),
    refetchInterval: 1000,
  });
  const runs = attempts.data ?? [];
  const working = runs.filter(({ status }) => isActive(status));
  const history = runs.filter(({ status }) => !isActive(status)).reverse();
  return <section className="pipeline-form pipeline-stack" aria-labelledby={heading}>
    <h4 id={heading} className="font-medium">Participants</h4>
    {attempts.isPending && <p role="status">Loading participants…</p>}
    {attempts.error && <p role="alert">{String(attempts.error)}</p>}
    {attempts.data && <section className="pipeline-stack" aria-labelledby={workingHeading}>
      <h5 id={workingHeading} className="text-sm font-medium">Working agents</h5>
      {working.length ? <ul className="pipeline-stack">{working.map((attempt) =>
        <ParticipantAttempt key={attempt.runId} attempt={attempt} openActivity={openActivity} />)}</ul>
        : <p className="text-sm text-muted-foreground">No active attempts.</p>}
    </section>}
    {history.length > 0 && <section className="pipeline-stack" aria-labelledby={historyHeading}>
      <h5 id={historyHeading} className="text-sm font-medium">Participant history</h5>
      <ul className="pipeline-stack">{history.map((attempt) =>
        <ParticipantAttempt key={attempt.runId} attempt={attempt} openActivity={openActivity} />)}</ul>
    </section>}
  </section>;
}
