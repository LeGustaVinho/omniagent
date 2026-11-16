import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Search } from "lucide-react";
import { useState } from "react";

export type RipgrepStatus = {
  configuredExecutable: string | null;
  executable: string | null;
  version: string | null;
  issue: string | null;
};

type SearchMatch = { path: string; line: number; excerpt: string };
type ProjectSearch = { status: "results"; matches: SearchMatch[]; truncated: boolean }
  | { status: "unavailable"; reason: string };

export const ripgrepStatusKey = ["ripgrep-status"] as const;
const noteLinePrefix = "omni-note-line:";

export function noteSearchTarget(path: string, line: number, request: number) {
  return noteLinePrefix + encodeURIComponent(JSON.stringify([path, line, request]));
}

export function readNoteSearchTarget(target: string): { path: string; line?: number } | undefined {
  if (!target.startsWith(noteLinePrefix)) return { path: target };
  try {
    const parts: unknown = JSON.parse(decodeURIComponent(target.slice(noteLinePrefix.length)));
    if (!Array.isArray(parts) || parts.length !== 3) return;
    const [path, line] = parts;
    if (typeof path === "string" && typeof line === "number" && Number.isSafeInteger(line) && line > 0) return { path, line };
  } catch {
    return undefined;
  }
}

export function useRipgrepStatus() {
  return useQuery({
    queryKey: ripgrepStatusKey,
    queryFn: () => invoke<RipgrepStatus>("ripgrep_status"),
    retry: false,
    staleTime: 30_000,
  });
}

export function ProjectFileSearch({ checkoutId, onOpen }: { checkoutId: string; onOpen: (path: string, line?: number) => void }) {
  const [query, setQuery] = useState("");
  const [submitted, setSubmitted] = useState<{ checkoutId: string; query: string; sequence: number }>();
  const ripgrep = useRipgrepStatus();
  const request = submitted?.checkoutId === checkoutId ? submitted : undefined;
  const results = useQuery({
    queryKey: ["project-file-search", checkoutId, request?.sequence, request?.query],
    queryFn: () => invoke<ProjectSearch>("search_repository_files", { checkoutId, query: request!.query }),
    enabled: request !== undefined,
    retry: false,
    staleTime: Infinity,
  });
  const answer = request ? results.data : undefined;
  const found = answer?.status === "results" ? answer : undefined;
  const issue = answer?.status === "unavailable" ? answer.reason : ripgrep.data?.issue;

  return <form className="file-search" aria-label="Project file search" onSubmit={(event) => {
    event.preventDefault();
    if (!query || !ripgrep.data?.executable || results.isFetching) return;
    setSubmitted({ checkoutId, query, sequence: (submitted?.sequence ?? 0) + 1 });
  }}>
    <label className="file-field">
      <Search size={15} aria-hidden="true" />
      <input type="search" autoFocus value={query} onChange={(event) => setQuery(event.target.value)}
        placeholder="Search file contents" aria-label="Search file contents" />
    </label>
    <button type="submit" className="fw-btn fw-outlined" disabled={!query || !ripgrep.data?.executable || results.isFetching}>Search</button>
    {ripgrep.isPending && <p className="file-help" role="status">Checking ripgrep…</p>}
    {ripgrep.error && <p className="file-help" role="alert">{String(ripgrep.error)}</p>}
    {issue && <p className="file-help" role="alert">{issue} Open System Settings to check the executable path.</p>}
    {request && results.isFetching && <p className="file-help" role="status">Searching this checkout…</p>}
    {request && results.error && <p className="file-help" role="alert">{String(results.error)}</p>}
    {found && <>
      <p className="file-help" role="status">
        {found.matches.length === 0 && !found.truncated ? "No matching lines." : `${found.matches.length} matching lines.`}
        {found.truncated && " Search stopped early. Narrow the query for more complete results."}
      </p>
      <ul className="file-search-results">
        {found.matches.map(({ path, line, excerpt }) => <li key={`${path}:${line}`}>
          <button type="button" onClick={() => onOpen(path, line)} title={path}>
            <strong>{path} <span>· {line}</span></strong>
            <span>{excerpt}</span>
          </button>
        </li>)}
      </ul>
    </>}
  </form>;
}
