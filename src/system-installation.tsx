import { useQuery } from "@tanstack/react-query";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import "./settings-facts.css";

export function SystemInstallation() {
  const runtime = useQuery({
    queryKey: ["runtime-status"], queryFn: () => invoke<string>("runtime_status"),
  });
  const build = useQuery({
    queryKey: ["application-version"], queryFn: getVersion, staleTime: Infinity,
  });
  return <dl className="settings-facts">
    <dt>Build</dt><dd>{build.data ?? (build.isError ? "Unavailable" : "Loading…")}</dd>
    <dt>Runtime</dt><dd>{runtime.data ?? (runtime.isError ? "Unavailable" : "Checking…")}</dd>
    <dt>Data directory</dt><dd>Local Windows application data</dd>
  </dl>;
}
