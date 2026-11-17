import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { CheckCircle2, CircleAlert } from "lucide-react";
import type { Provider } from "@/bindings/Provider";
import { Button } from "@/components/ui/button";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import { ripgrepStatusKey } from "@/ide-file-search";

export const desktopCliPrerequisitesKey = ["desktop-cli-prerequisites"] as const;
export const providerExecutableLabels: Record<Provider, string> = {
  codex: "Codex",
  claude: "Claude Code",
  antigravity: "Gemini via agy",
};

export type CliPrerequisite = { executable: string | null; version: string | null; issue: string | null };
type ProviderPrerequisite = { provider: Provider; configuredExecutable: string | null; status: CliPrerequisite };
type DesktopCliPrerequisiteStatus = { git: CliPrerequisite; providers: ProviderPrerequisite[] };

export function useDesktopCliPrerequisites() {
  return useQuery({
    queryKey: desktopCliPrerequisitesKey,
    queryFn: () => invoke<DesktopCliPrerequisiteStatus>("desktop_cli_prerequisites"),
    staleTime: 30_000,
    retry: false,
  });
}

export function ExecutableInstallation({ status, label, provider, refresh, compact }: { status: CliPrerequisite; label: string;
  provider?: Provider; refresh?: () => void; compact?: boolean }) {
  const { feedback, run, copy } = useContextActionFeedback();
  const { executable, version, issue } = status;
  const summary = [label, version, executable, issue].filter(Boolean).join("\n");
  return <>
    <ContextActions label={`${label} installation`} groups={[[
      ...(executable ? [{ label: "Show executable in File Explorer", run: () => void run(() =>
        invoke("reveal_desktop_cli_executable", { provider: provider ?? null, expected: executable })) }] : []),
      ...(refresh ? [{ label: "Check installed executables", run: refresh }] : []),
    ], [
      ...(executable ? [{ label: "Copy executable path", run: () => copy(executable) }] : []),
      ...(version ? [{ label: "Copy version", run: () => copy(version) }] : []),
      { label: "Copy installation details", run: () => copy(summary) },
    ]]}>
      <div className="min-w-0">
        <span className="executable-status" data-attention={Boolean(issue || !executable || !version)}>
          {issue || !executable || !version ? <CircleAlert size={14} aria-hidden /> : <CheckCircle2 size={14} aria-hidden />}
          {version ?? (executable ? "Version unverified" : "Unavailable")}
        </span>
        {executable && (compact
          ? <details><summary>Executable path</summary><code className="block break-all">{executable}</code></details>
          : <code className="block break-all">{executable}</code>)}
        {issue && <p className="settings-result settings-result-error">{issue}</p>}
      </div>
    </ContextActions>
    {feedback && <p className="settings-result" role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
  </>;
}

export function DesktopCliPrerequisites() {
  const status = useDesktopCliPrerequisites();
  const client = useQueryClient();
  function refresh() {
    void client.invalidateQueries({ queryKey: desktopCliPrerequisitesKey });
    void client.invalidateQueries({ queryKey: ripgrepStatusKey });
  }
  return <section aria-label="Desktop CLI installation">
    {status.isPending && <p role="status">Checking installed executables…</p>}
    {status.error && <p role="alert">{String(status.error)}</p>}
    {status.data && <dl className="settings-facts">
      <dt>Git for Windows</dt><dd><ExecutableInstallation status={status.data.git} label="Git for Windows" refresh={refresh} /></dd>
      {status.data.providers.map(({ provider, status: executable }) => {
        const label = providerExecutableLabels[provider];
        return <ExecutableRow key={provider} label={label} status={executable} provider={provider} refresh={refresh} />;
      })}
    </dl>}
    <Button type="button" variant="outline" disabled={status.isFetching} onClick={refresh}>Check installed executables</Button>
  </section>;
}

function ExecutableRow(props: Parameters<typeof ExecutableInstallation>[0]) {
  return <><dt>{props.label}</dt><dd><ExecutableInstallation {...props} compact /></dd></>;
}
