import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { Search } from "lucide-react";
import { useState } from "react";
import type { FileSearchResult } from "@/ide-knowledge-data";

export type RipgrepStatus = {
  configuredExecutable: string | null;
  executable: string | null;
  version: string | null;
  issue: string | null;
};

type ProjectSearch = { status: "results"; matches: FileSearchResult[]; truncated: boolean }
  | { status: "unavailable"; reason: string };
type OpenMatch = (path: string, line: number) => void;

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

function FileSearchResults({ results, onOpen }: { results: readonly FileSearchResult[]; onOpen: OpenMatch }) {
  if (!results.length) return null;
  return <ul className="file-search-results">
    {results.map(({ path, line, excerpt }, index) => <li key={`${path}:${line}:${index}`}>
      <button type="button" onClick={() => onOpen(path, line)} title={`${path} · Line ${line}`}>
        <strong>{path} <span>· {line}</span></strong>
        <span>{excerpt}</span>
      </button>
    </li>)}
  </ul>;
}

function FileSearchField({ label, query, onChange }: { label: string; query: string; onChange: (query: string) => void }) {
  return <label className="file-field">
    <Search size={15} aria-hidden="true" />
    <input type="search" autoFocus value={query} onChange={(event) => onChange(event.currentTarget.value)}
      placeholder={label} aria-label={label} />
  </label>;
}

export function ExampleFileSearch({ query, setQuery, results, onOpen }: {
  query: string;
  setQuery: (query: string) => void;
  results: readonly FileSearchResult[];
  onOpen: OpenMatch;
}) {
  return <div className="file-search">
    <FileSearchField label="Search files and content" query={query} onChange={setQuery} />
    {query.trim() && <p className="file-help" role="status">{results.length ? `${results.length} matches` : "No files match your search"}</p>}
    <FileSearchResults results={results} onOpen={onOpen} />
  </div>;
}

export function ProjectFileSearch({ checkoutId, onOpen }: { checkoutId: string; onOpen: OpenMatch }) {
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
    <FileSearchField label="Search file contents" query={query} onChange={setQuery} />
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
      <FileSearchResults results={found.matches} onOpen={onOpen} />
    </>}
  </form>;
}
