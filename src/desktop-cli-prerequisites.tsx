import { useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
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

export function ExecutableInstallation({ status }: { status: CliPrerequisite }) {
  return <>
    <span>{status.version ?? (status.executable ? "Version unverified" : "Unavailable")}</span>
    {status.executable && <code className="block break-all">{status.executable}</code>}
    {status.issue && <p className="settings-result settings-result-error">{status.issue}</p>}
  </>;
}

export function DesktopCliPrerequisites() {
  const status = useDesktopCliPrerequisites();
  const client = useQueryClient();
  return <section aria-label="Desktop CLI installation">
    {status.isPending && <p role="status">Checking installed executables…</p>}
    {status.error && <p role="alert">{String(status.error)}</p>}
    {status.data && <dl className="settings-facts">
      <dt>Git for Windows</dt><dd><ExecutableInstallation status={status.data.git} /></dd>
      {status.data.providers.map(({ provider, status: executable }) =>
        <ExecutableRow key={provider} label={providerExecutableLabels[provider]} status={executable} />)}
    </dl>}
    <Button type="button" variant="outline" disabled={status.isFetching} onClick={() => {
      void client.invalidateQueries({ queryKey: desktopCliPrerequisitesKey });
      void client.invalidateQueries({ queryKey: ripgrepStatusKey });
    }}>Check installed executables</Button>
  </section>;
}

function ExecutableRow({ label, status }: { label: string; status: CliPrerequisite }) {
  return <><dt>{label}</dt><dd><ExecutableInstallation status={status} /></dd></>;
}
