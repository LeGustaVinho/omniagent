import { useQuery } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { ChevronRight, FileText, Search } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import type { FileSearchResult } from "@/ide-knowledge-data";
import { ContextActions, type MenuGroups } from "@/components/ui/context-actions";

export type RipgrepStatus = {
  configuredExecutable: string | null;
  executable: string | null;
  version: string | null;
  issue: string | null;
};

type ProjectSearch = { status: "results"; matches: FileSearchResult[]; truncated: boolean }
  | { status: "unavailable"; reason: string };
type OpenMatch = (path: string, line: number, column?: number) => void;
type MatchActions = (match: FileSearchResult) => MenuGroups;
type FileActions = (matches: readonly FileSearchResult[]) => MenuGroups;
export type FileSearchScope = { kind: "file" | "folder"; path: string };
export type EditorSearchRequest = { checkoutId: string; text: string; sequence: number };

export function isProjectSearchText(text: string) {
  return text.length > 0 && text.length <= 4096 && !/[\r\n\0]/.test(text);
}

export function matchReference({ path, line, columns }: FileSearchResult, column = columns?.[0]) {
  return column === undefined ? `${path}:${line}` : `${path}:${line}:${column}`;
}

export const ripgrepStatusKey = ["ripgrep-status"] as const;
const noteLinePrefix = "omni-note-line:";
const folderSearchPrefix = "omni-folder-search:";

export function folderSearchTarget(path: string) {
  return folderSearchPrefix + path;
}

export function readFolderSearchTarget(target?: string) {
  if (target?.startsWith(folderSearchPrefix)) return target.slice(folderSearchPrefix.length);
}

export function noteSearchTarget(path: string, line: number | undefined, request: number, side = false) {
  return noteLinePrefix + encodeURIComponent(JSON.stringify(side ? [path, line, request, true] : [path, line, request]));
}

export function readNoteSearchTarget(target: string): { path: string; line?: number; side?: boolean } | undefined {
  if (!target.startsWith(noteLinePrefix)) return { path: target };
  try {
    const parts: unknown = JSON.parse(decodeURIComponent(target.slice(noteLinePrefix.length)));
    if (!Array.isArray(parts) || (parts.length !== 3 && (parts.length !== 4 || parts[3] !== true))) return;
    const [path, line, , side] = parts;
    if (typeof path === "string" && (line === null || typeof line === "number" && Number.isSafeInteger(line) && line > 0)) {
      return { path, line: line ?? undefined, ...(side ? { side: true } : {}) };
    }
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

function FileSearchGroup({ path, matches, onOpen, actions, fileActions }: {
  path: string; matches: readonly FileSearchResult[]; onOpen: OpenMatch; actions: MatchActions; fileActions: FileActions;
}) {
  const id = useId();
  const [expanded, setExpanded] = useState(true);
  const occurrences = matches.reduce((total, match) => total + (match.occurrences ?? 1), 0);
  const count = `${occurrences} ${occurrences === 1 ? "match" : "matches"}`;
  return <li>
    <ContextActions label={`matches in ${path}`} groups={fileActions(matches)}>
      <button type="button" className="file-search-group" aria-expanded={expanded} aria-controls={id}
        aria-label={`${path}, ${count}`} onClick={() => setExpanded(!expanded)} title={path}>
        <ChevronRight size={13} className="file-search-chevron" aria-hidden /><FileText size={14} aria-hidden />
        <strong>{path}</strong><small>{count}</small>
      </button>
    </ContextActions>
    <ul id={id} className="file-search-hits" hidden={!expanded}>
      {matches.map((match, index) => <li key={`${matchReference(match)}:${index}`}>
        <ContextActions label={matchReference(match)} groups={actions(match)}>
          <button type="button" className="file-search-match" onClick={() => onOpen(path, match.line, match.columns?.[0])}
            title={matchReference(match)} aria-label={`${path} · ${match.line}${match.columns?.length ? `:${match.columns[0]}` : ""} ${match.excerpt}`}>
            <strong>{match.line}{match.columns?.length ? `:${match.columns[0]}` : ""}</strong>
            <span>{match.excerpt}</span>
          </button>
        </ContextActions>
      </li>)}
    </ul>
  </li>;
}

function FileSearchResults({ results, onOpen, actions, fileActions }: {
  results: readonly FileSearchResult[]; onOpen: OpenMatch; actions: MatchActions; fileActions: FileActions;
}) {
  if (!results.length) return null;
  const grouped = new Map<string, FileSearchResult[]>();
  for (const match of results) {
    const matches = grouped.get(match.path);
    if (matches) matches.push(match);
    else grouped.set(match.path, [match]);
  }
  return <ul className="file-search-results">{Array.from(grouped, ([path, matches]) =>
    <FileSearchGroup key={path} path={path} matches={matches} onOpen={onOpen} actions={actions} fileActions={fileActions} />)}</ul>;
}

function FileSearchField({ label, query, onChange, scope }: { label: string; query: string; onChange: (query: string) => void; scope?: FileSearchScope }) {
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!scope) return;
    // Let the closing context menu restore focus before moving it to search.
    let focusTimer: number | undefined;
    const frame = requestAnimationFrame(() => { focusTimer = window.setTimeout(() => input.current?.focus()); });
    return () => {
      cancelAnimationFrame(frame);
      window.clearTimeout(focusTimer);
    };
  }, [scope]);
  return <label className="file-field">
    <Search size={15} aria-hidden="true" />
    <input ref={input} type="search" autoFocus value={query} onChange={(event) => onChange(event.currentTarget.value)}
      placeholder={label} aria-label={label} />
  </label>;
}

function ScopeLabel({ scope, onClear }: { scope?: FileSearchScope; onClear?: () => void }) {
  if (!scope?.path) return null;
  return <div className="file-search-scope">
    <p className="file-help">Search in {scope.path}</p>
    {onClear && <button type="button" className="fw-btn" onClick={onClear}>Search entire worktree</button>}
  </div>;
}

export function ExampleFileSearch({ query, setQuery, results, onOpen, actions, fileActions, scope, onClear, literalContent }: {
  query: string;
  setQuery: (query: string) => void;
  results: readonly FileSearchResult[];
  onOpen: OpenMatch;
  actions: MatchActions;
  fileActions: FileActions;
  scope?: FileSearchScope;
  onClear?: () => void;
  literalContent?: boolean;
}) {
  return <div className="file-search">
    <ScopeLabel scope={scope} onClear={onClear} />
    <FileSearchField label="Search files and content" query={query} onChange={setQuery} scope={scope} />
    {(literalContent ? query : query.trim()) && <p className="file-help" role="status">{results.length ? `${results.length} matches` : "No files match your search"}</p>}
    <FileSearchResults results={results} onOpen={onOpen} actions={actions} fileActions={fileActions} />
  </div>;
}

export function ProjectFileSearch({ checkoutId, onOpen, scope, onClear, editorSearch, actions = () => [], fileActions = () => [] }: {
  checkoutId: string; onOpen: OpenMatch; scope?: FileSearchScope; onClear?: () => void; actions?: MatchActions; fileActions?: FileActions;
  editorSearch?: EditorSearchRequest;
}) {
  const [query, setQuery] = useState("");
  const [submitted, setSubmitted] = useState<{ checkoutId: string; query: string; sequence: string; editorSearch?: EditorSearchRequest }>();
  const formSequence = useRef(0);
  const selection = editorSearch?.checkoutId === checkoutId ? editorSearch : undefined;
  const ripgrep = useRipgrepStatus();
  useEffect(() => {
    if (!selection || !isProjectSearchText(selection.text)) return;
    setQuery(selection.text);
    setSubmitted({ checkoutId, query: selection.text, sequence: `editor:${selection.sequence}`, editorSearch: selection });
  }, [checkoutId, selection]);
  const request = submitted?.checkoutId === checkoutId && (!selection || submitted.editorSearch === selection) ? submitted : undefined;
  const results = useQuery({
    queryKey: ["project-file-search", checkoutId, scope?.kind, scope?.path, request?.sequence, request?.query],
    queryFn: () => invoke<ProjectSearch>("search_repository_files", { checkoutId, query: request!.query,
      ...(scope?.kind === "file" ? { relativeFile: scope.path } : scope?.path ? { relativeDirectory: scope.path } : {}) }),
    enabled: request !== undefined && Boolean(ripgrep.data?.executable),
    retry: false,
    staleTime: Infinity,
  });
  const answer = request ? results.data : undefined;
  const found = answer?.status === "results" ? answer : undefined;
  const issue = answer?.status === "unavailable" ? answer.reason : ripgrep.data?.issue;

  return <form className="file-search" aria-label="Project file search" onSubmit={(event) => {
    event.preventDefault();
    if (!query || !ripgrep.data?.executable || results.isFetching) return;
    setSubmitted({ checkoutId, query, sequence: `form:${++formSequence.current}`, editorSearch: selection });
  }}>
    <ScopeLabel scope={scope} onClear={onClear} />
    <FileSearchField label="Search file contents" query={query} onChange={setQuery} scope={scope} />
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
      <FileSearchResults results={found.matches} onOpen={onOpen} actions={actions} fileActions={fileActions} />
    </>}
  </form>;
}
