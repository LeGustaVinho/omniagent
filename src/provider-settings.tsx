import { invoke } from "@tauri-apps/api/core";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import type { CliProbe } from "@/bindings/CliProbe";
import type { ConfiguredExecutable } from "@/bindings/ConfiguredExecutable";
import type { Provider } from "@/bindings/Provider";
import { Button } from "@/components/ui/button";

const PROVIDERS: Record<Provider, string> = {
  codex: "Codex",
  claude: "Claude Code",
  antigravity: "Antigravity",
};

const AUTH_LABELS: Record<CliProbe["authentication"], string> = {
  authenticated: "Signed in",
  required: "Sign-in required",
  unknown: "Unverified",
};

const PROTOCOL_LABELS: Record<CliProbe["protocol"], string> = {
  unchecked: "Unchecked",
  verified: "Verified",
  failed: "Negotiation failed",
};

export function ProviderSettingsPanel() {
  const profiles = useQuery({
    queryKey: ["cli-executables"],
    queryFn: () => invoke<ConfiguredExecutable[]>("cli_executables"),
    staleTime: Infinity,
  });
  return (
    <section className="h-full overflow-auto p-6">
      <h1 className="mb-5 text-xl font-semibold tracking-tight">Provider settings</h1>
      {profiles.isPending && <p role="status" className="text-sm text-muted-foreground">Loading executable paths…</p>}
      {profiles.isError && <p role="alert" className="text-sm text-red-300">{String(profiles.error)}</p>}
      <div className="grid max-w-3xl gap-4">
        {profiles.data?.map((profile) => <ExecutableSettings key={profile.provider} profile={profile} />)}
      </div>
    </section>
  );
}

function ExecutableSettings({ profile }: { profile: ConfiguredExecutable }) {
  const queryClient = useQueryClient();
  const [configured, setConfigured] = useState(profile.executable);
  const [path, setPath] = useState(profile.executable ?? "");
  const [probe, setProbe] = useState<CliProbe | null>(null);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const { provider } = profile;

  async function run(operation: () => Promise<void>) {
    setError("");
    setPending(true);
    try {
      await operation();
    } catch (failure) {
      setError(String(failure));
    } finally {
      setPending(false);
    }
  }

  async function save(executable: string | null) {
    const updated = await invoke<ConfiguredExecutable>("set_cli_executable", { provider, executable });
    queryClient.setQueryData<ConfiguredExecutable[]>(["cli-executables"], (current) =>
      current?.map((entry) => entry.provider === provider ? updated : entry));
    setConfigured(updated.executable);
    setPath(updated.executable ?? "");
    setProbe(null);
  }

  function submit(event: FormEvent) {
    event.preventDefault();
    void run(() => save(path.trim()));
  }

  return (
    <section className="rounded-lg border border-border bg-card p-4">
      <h2 className="text-base font-semibold">{PROVIDERS[provider]}</h2>
      <form onSubmit={submit} className="mt-3 flex flex-wrap items-end gap-2">
        <label className="min-w-52 flex-1 text-sm">
          Executable path
          <input name={provider} value={path} onChange={(event) => setPath(event.target.value)}
            placeholder="Use PATH" className="mt-1 block w-full rounded-md border border-border bg-background p-2 font-mono text-xs" />
        </label>
        <Button type="submit" disabled={pending || !path.trim() || path.trim() === configured}>Save</Button>
        <Button type="button" variant="outline" disabled={pending || configured === null}
          onClick={() => void run(() => save(null))}>Use PATH</Button>
        <Button type="button" variant="outline" disabled={pending || path.trim() !== (configured ?? "")}
          onClick={() => void run(async () => setProbe(await invoke<CliProbe>("probe_cli_executable", { provider })))}>
          {pending ? "Working…" : "Check CLI"}
        </Button>
      </form>
      {error && <p role="alert" className="mt-2 text-sm text-red-300">{error}</p>}
      {probe && (
        <dl className="mt-3 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-sm">
          <dt className="text-muted-foreground">Installation</dt>
          <dd>{probe.version ?? (probe.executable ? "Detected, version unverified" : "Executable not found")}</dd>
          <dt className="text-muted-foreground">Protocol</dt><dd>{PROTOCOL_LABELS[probe.protocol]}</dd>
          <dt className="text-muted-foreground">Authentication</dt><dd>{AUTH_LABELS[probe.authentication]}</dd>
          <dt className="text-muted-foreground">Effective permissions</dt><dd>Unverified</dd>
        </dl>
      )}
    </section>
  );
}
