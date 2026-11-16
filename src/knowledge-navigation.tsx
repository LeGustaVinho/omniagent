import { Search } from "lucide-react";
import { Tooltip } from "radix-ui";
import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { fileName } from "@/ide-knowledge-data";

export type KnowledgeLocation = {
  path: string;
  line?: number | null;
  anchor?: string | null;
};

export type KnowledgeResolvedTarget = KnowledgeLocation & {
  title?: string | null;
  snippet?: string | null;
};

export type KnowledgeSearchHit = KnowledgeResolvedTarget;
export type KnowledgeOutlineEntry = KnowledgeLocation & { label: string; level: number };
export type KnowledgeBacklink = { source: KnowledgeResolvedTarget; label?: string };

export type KnowledgeOutgoingLink = {
  label: string;
  source: KnowledgeLocation;
  rawTarget: string;
  resolution:
    | { kind: "resolved"; target: KnowledgeResolvedTarget }
    | { kind: "missing" }
    | { kind: "ambiguous"; candidates: readonly KnowledgeResolvedTarget[] };
};

type OpenNote = (location: KnowledgeLocation) => void;
type RepairLink = (link: KnowledgeOutgoingLink) => void;
type NavigationState = { pending?: boolean; error?: string | null };

function openLocation(open: OpenNote, { path, line, anchor }: KnowledgeLocation) {
  open({ path, line, anchor });
}

function locationLabel({ path, line, anchor }: KnowledgeLocation) {
  return path + (anchor ? `#${anchor}` : "") + (line == null ? "" : ` · Line ${line}`);
}

function locationKey({ path, line, anchor }: KnowledgeLocation) {
  return JSON.stringify([path, line ?? null, anchor ?? null]);
}

function NavigationMessage({ pending, error, loading, children }: NavigationState & { loading: string; children: ReactNode }) {
  if (error) return <p role="alert" className="file-help">{error}</p>;
  if (pending) return <p role="status" className="file-help">{loading}</p>;
  return children;
}

export function KnowledgeSearch({
  query, onQueryChange, results, onOpen, pending, error, focusRequest,
}: NavigationState & {
  query: string;
  onQueryChange: (query: string) => void;
  results: readonly KnowledgeSearchHit[];
  onOpen: OpenNote;
  focusRequest?: number;
}) {
  const input = useRef<HTMLInputElement>(null);
  const listId = useId();
  const [show, setShow] = useState(true);
  const [selection, setSelection] = useState<{ query: string; location: string }>();
  const hasResults = !pending && !error && results.length > 0;
  const expanded = show && hasResults;
  const selected = selection?.query === query
    ? Math.max(0, results.findIndex((result) => locationKey(result) === selection.location)) : 0;
  const activeId = expanded ? `${listId}-${selected}` : undefined;

  useEffect(() => {
    if (!input.current || focusRequest === undefined) return;
    input.current.focus();
    input.current.select();
    setShow(true);
  }, [focusRequest]);

  useEffect(() => {
    if (activeId) document.getElementById(activeId)?.scrollIntoView?.({ block: "nearest" });
  }, [activeId]);

  const choose = (result: KnowledgeSearchHit) => {
    setShow(false);
    openLocation(onOpen, result);
  };
  const keyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.nativeEvent.isComposing || event.altKey || event.ctrlKey || event.metaKey) return;
    if (event.key === "Escape" && expanded) {
      event.preventDefault();
      event.stopPropagation();
      setShow(false);
    } else if (event.key === "Enter" && expanded) {
      event.preventDefault();
      choose(results[selected]!);
    } else if (hasResults && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
      event.preventDefault();
      const delta = event.key === "ArrowDown" ? 1 : -1;
      const next = expanded ? Math.min(Math.max(selected + delta, 0), results.length - 1)
        : delta > 0 ? 0 : results.length - 1;
      setSelection({ query, location: locationKey(results[next]!) });
      setShow(true);
    }
  };

  return <div className="file-search">
    <label className="file-field">
      <Search size={15} aria-hidden />
      <input ref={input} type="search" role="combobox" aria-label="Find notes and content" placeholder="Find notes and content"
        value={query} aria-autocomplete="list" aria-expanded={expanded}
        aria-controls={expanded ? listId : undefined} aria-activedescendant={activeId}
        onFocus={() => setShow(true)} onBlur={() => setShow(false)} onKeyDown={keyDown}
        onChange={(event) => {
          setSelection(undefined);
          setShow(true);
          onQueryChange(event.currentTarget.value);
        }} />
    </label>
    <NavigationMessage pending={pending} error={error} loading="Searching notes…">
      {query.trim() && !results.length && <p role="status" className="file-help">No notes match your search.</p>}
      {expanded && <ul id={listId} role="listbox" aria-label="Note search results" className="file-search-results">
        {results.map((result, index) => <li role="presentation" key={`${locationKey(result)}:${index}`}>
          <button type="button" role="option" id={`${listId}-${index}`} tabIndex={-1}
            className="aria-selected:bg-muted"
            aria-selected={index === selected} title={locationLabel(result)}
            onMouseDown={(event) => event.preventDefault()}
            onPointerMove={() => {
              if (index !== selected) setSelection({ query, location: locationKey(result) });
            }}
            onClick={() => choose(result)}>
            <strong>{result.title || fileName(result.path)}</strong>
            <span>{locationLabel(result)}</span>
            {result.snippet && <span>{result.snippet}</span>}
          </button>
        </li>)}
      </ul>}
    </NavigationMessage>
  </div>;
}

export function KnowledgeOutline({ entries, onOpen, pending, error }: NavigationState & {
  entries: readonly KnowledgeOutlineEntry[];
  onOpen: OpenNote;
}) {
  return <section aria-label="Outline">
    <h3>Outline</h3>
    <NavigationMessage pending={pending} error={error} loading="Loading outline…">
      {entries.length ? <ul>
        {entries.map((entry) => <li key={locationLabel(entry)}>
          <button type="button" title={locationLabel(entry)}
            style={{ paddingInlineStart: 5 + Math.min(Math.max(entry.level - 1, 0), 5) * 12 }}
            onClick={() => openLocation(onOpen, entry)}>{entry.label}</button>
        </li>)}
      </ul> : <p className="file-help">No headings in this note.</p>}
    </NavigationMessage>
  </section>;
}

function TargetDescription({ location }: { location: KnowledgeResolvedTarget }) {
  return <>
    {location.title && <strong>{location.title}</strong>}
    <span>{locationLabel(location)}</span>
    {location.snippet && <span>{location.snippet}</span>}
  </>;
}

function PreviewButton({ children, preview, onClick, className, title }: {
  children: ReactNode;
  preview: ReactNode;
  onClick: () => void;
  className?: string;
  title?: string;
}) {
  return <Tooltip.Provider delayDuration={200}>
    <Tooltip.Root>
      <Tooltip.Trigger asChild><button type="button" className={className} title={title} onClick={onClick}>{children}</button></Tooltip.Trigger>
      <Tooltip.Portal>
        <Tooltip.Content sideOffset={6} className="work-tooltip z-50 grid max-w-64 gap-1 whitespace-normal [overflow-wrap:anywhere]">
          {preview}
        </Tooltip.Content>
      </Tooltip.Portal>
    </Tooltip.Root>
  </Tooltip.Provider>;
}

export function KnowledgeLinkPreview({ link, onOpen, onRepair, className, children }: {
  link: KnowledgeOutgoingLink;
  onOpen: OpenNote;
  onRepair?: RepairLink;
  className?: string;
  children?: ReactNode;
}) {
  const resolution = link.resolution;
  const target = resolution.kind === "resolved" ? resolution.target : undefined;
  const status = resolution.kind === "missing" ? "Missing" : resolution.kind === "ambiguous" ? "Ambiguous" : undefined;
  return <PreviewButton className={className} title={target ? locationLabel(target) : link.rawTarget}
    preview={target ? <TargetDescription location={target} /> : <>
      <strong>{status} note reference</strong>
      <span>{link.rawTarget}</span>
      <span>{onRepair ? "Open the link repair controls." : "Jump to the link in its source note."}</span>
    </>}
    onClick={() => {
      if (target) openLocation(onOpen, target);
      else if (onRepair) onRepair(link);
      else openLocation(onOpen, link.source);
    }}>
    {children ?? link.label}{status && ` · ${status}`}
  </PreviewButton>;
}

export function KnowledgeLinks({ outgoing, backlinks, onOpen, onRepair, pending, error }: NavigationState & {
  outgoing: readonly KnowledgeOutgoingLink[];
  backlinks: readonly KnowledgeBacklink[];
  onOpen: OpenNote;
  onRepair?: RepairLink;
}) {
  return <NavigationMessage pending={pending} error={error} loading="Loading note links…">
    <section aria-label="Outgoing links">
      <h3>Outgoing links</h3>
      {outgoing.length ? <ul>
        {outgoing.map((link, index) => <li key={`${locationLabel(link.source)}:${link.rawTarget}:${index}`}>
          <KnowledgeLinkPreview link={link} onOpen={onOpen} onRepair={onRepair} />
          {link.resolution.kind === "ambiguous" && <details className="px-2 text-xs">
            <summary className="cursor-pointer">Candidate notes</summary>
            <ul>{link.resolution.candidates.map((candidate) => <li key={locationLabel(candidate)}>
              <PreviewButton title={locationLabel(candidate)} preview={<TargetDescription location={candidate} />}
                onClick={() => openLocation(onOpen, candidate)}>{candidate.path}</PreviewButton>
            </li>)}</ul>
          </details>}
        </li>)}
      </ul> : <p className="file-help">No outgoing links.</p>}
    </section>
    <section aria-label="Backlinks">
      <h3>Backlinks</h3>
      {backlinks.length ? <ul>
        {backlinks.map(({ source, label }, index) => <li key={`${locationLabel(source)}:${index}`}>
          <PreviewButton title={locationLabel(source)} preview={<TargetDescription location={source} />}
            onClick={() => openLocation(onOpen, source)}>{label || source.title || fileName(source.path)}</PreviewButton>
        </li>)}
      </ul> : <p className="file-help">No incoming links.</p>}
    </section>
  </NavigationMessage>;
}
