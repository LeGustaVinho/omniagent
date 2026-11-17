import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { CheckCircle2, CircleAlert } from "lucide-react";
import type { Provider } from "@/bindings/Provider";
import { Button } from "@/components/ui/button";
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

export function ExecutableInstallation({ status, compact }: { status: CliPrerequisite; compact?: boolean }) {
  const needsAttention = Boolean(status.issue || !status.executable || !status.version);
  return <>
    <span className="executable-status" data-attention={needsAttention}>
      {needsAttention ? <CircleAlert size={14} aria-hidden="true" /> : <CheckCircle2 size={14} aria-hidden="true" />}
      {status.version ?? (status.executable ? "Version unverified" : "Unavailable")}
    </span>
    {status.executable && (compact
      ? <details><summary>Executable path</summary><code className="block break-all">{status.executable}</code></details>
      : <code className="block break-all">{status.executable}</code>)}
    {status.issue && <p className="settings-result settings-result-error">{status.issue}</p>}
  </>;
}

export function DesktopCliPrerequisites() {
  const status = useDesktopCliPrerequisites();
  const client = useQueryClient();
  const entries = status.data && [
    { label: "Git for Windows", status: status.data.git },
    ...status.data.providers.map(({ provider, status }) => ({ label: providerExecutableLabels[provider], status })),
  ].sort((a, b) => Number(Boolean(b.status.issue || !b.status.version)) - Number(Boolean(a.status.issue || !a.status.version)));
  return <section aria-label="Desktop CLI installation">
    {status.isPending && <p role="status">Checking installed executables…</p>}
    {status.error && <p role="alert">{String(status.error)}</p>}
    {entries && <dl className="settings-facts">
      {entries.map(({ label, status }) => <ExecutableRow key={label} label={label} status={status} />)}
    </dl>}
    <Button type="button" variant="outline" disabled={status.isFetching} onClick={() => {
      void client.invalidateQueries({ queryKey: desktopCliPrerequisitesKey });
      void client.invalidateQueries({ queryKey: ripgrepStatusKey });
    }}>Check installed executables</Button>
  </section>;
}

function ExecutableRow({ label, status }: { label: string; status: CliPrerequisite }) {
  return <><dt>{label}</dt><dd><ExecutableInstallation status={status} compact /></dd></>;
}
