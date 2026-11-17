import { useQuery } from "@tanstack/react-query";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { ContextActions, useContextActionFeedback } from "@/components/ui/context-actions";
import "./settings-facts.css";

export function SystemInstallation() {
  const { copy, feedback } = useContextActionFeedback();
  const runtime = useQuery({
    queryKey: ["runtime-status"], queryFn: () => invoke<string>("runtime_status"),
  });
  const build = useQuery({
    queryKey: ["application-version"], queryFn: getVersion, staleTime: Infinity,
  });
  const { data: version } = build;
  const { data: status } = runtime;
  return <>
    <ContextActions label="OmniAgent installation details" groups={[[
      { label: "Refresh runtime status", disabled: runtime.isFetching, run: () => void runtime.refetch() },
    ], [
      ...(version ? [{ label: "Copy application version", run: () => copy(version) }] : []),
      ...(status ? [{ label: "Copy runtime status", run: () => copy(status) }] : []),
      ...(version && status ? [{ label: "Copy installation summary", run: () => copy(`OmniAgent ${version}\n${status}`) }] : []),
    ]]}>
      <dl className="settings-facts min-w-0 flex-1">
        <dt>Build</dt><dd>{version ?? (build.isError ? "Unavailable" : "Loading…")}</dd>
        <dt>Runtime</dt><dd>{status ?? (runtime.isError ? "Unavailable" : "Checking…")}</dd>
        <dt>Data directory</dt><dd>Local Windows application data</dd>
      </dl>
    </ContextActions>
    {feedback && <p role={feedback.failed ? "alert" : "status"}>{feedback.text}</p>}
  </>;
}
