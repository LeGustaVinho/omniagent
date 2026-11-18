import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { Check, ChevronDown, FolderGit2, GitFork, Search, Star } from "lucide-react";
import { Popover } from "radix-ui";
import { useState } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { Button } from "@/components/ui/button";
import { ContextActions } from "@/components/ui/context-actions";
import { fileContextActions, gitCheckoutTarget, newWorktreeTarget, terminalFolderTarget } from "@/file-context-actions";
import type { WorkspaceDestination, WorkspaceId } from "@/workbench-contract";

function basename(path: string) {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
}

const checkoutAreas: [WorkspaceId, string][] = [["ide", "IDE"], ["knowledge", "Knowledge"], ["git", "Git"], ["conversations", "Conversations"]];
const favoriteProjectsKey = "omniagent-favorite-projects";

function readFavoriteProjects(): ReadonlySet<string> {
  try {
    const saved: unknown = JSON.parse(localStorage.getItem(favoriteProjectsKey) ?? "[]");
    if (Array.isArray(saved)) return new Set(saved.filter((id): id is string => typeof id === "string" && id !== ""));
  } catch {
    return new Set();
  }
  return new Set();
}

export function CheckoutSelector({ checkout, onSelect, disabled }: {
  checkout: ProjectCheckout | null;
  disabled?: boolean;
  onSelect: (checkout: ProjectCheckout, destination?: WorkspaceDestination) => Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const [query, setQuery] = useState("");
  const [status, setStatus] = useState("");
  const [favorites, setFavorites] = useState(readFavoriteProjects);
  const registered = useQuery({
    queryKey: ["registered-checkouts"],
    queryFn: () => invoke<ProjectCheckout[]>("registered_checkouts"),
    enabled: open,
  });
  const search = query.trim().toLowerCase();
  const results = (registered.data ?? []).filter(({ repositoryRoot, checkoutRoot }) =>
    [repositoryRoot, checkoutRoot].some((path) => path.toLowerCase().includes(search)))
    .sort((left, right) => Number(favorites.has(right.projectId)) - Number(favorites.has(left.projectId)));

  function toggleFavorite(project: ProjectCheckout) {
    const next = new Set(favorites);
    const removed = next.delete(project.projectId);
    if (!removed) next.add(project.projectId);
    setFavorites(next);
    setStatus("");
    setError("");
    try {
      localStorage.setItem(favoriteProjectsKey, JSON.stringify([...next]));
      const name = basename(project.repositoryRoot);
      setStatus(removed ? `Removed ${name} from project favorites.` : `Added ${name} to project favorites.`);
    } catch (failure) {
      setError(`Could not save project favorites. ${String(failure)}`);
    }
  }

  async function select(checked: ProjectCheckout, destination?: WorkspaceDestination) {
    setPending(true);
    setError("");
    setStatus("");
    try {
      if (destination) await onSelect(checked, destination);
      else await onSelect(checked);
      setOpen(false);
    } catch (failure) {
      setError(String(failure));
    } finally {
      setPending(false);
    }
  }

  return <Popover.Root open={open} onOpenChange={(next) => {
    if (!next && pending) return;
    if (next) { setQuery(""); setError(""); }
    setOpen(next);
  }}>
    <Popover.Trigger asChild>
      <Button variant={checkout ? "ghost" : "outline"} className={checkout ? "work-project-button" : undefined}
        disabled={disabled} aria-label={checkout ? "Switch repository or worktree" : "Registered repositories"}>
        <FolderGit2 size={15} className="shrink-0" />
        <span title={checkout?.checkoutRoot} className="truncate">{checkout ? basename(checkout.repositoryRoot) : "Registered repositories"}
          {checkout && checkout.checkoutRoot !== checkout.repositoryRoot && " / " + basename(checkout.checkoutRoot)}
        </span>
        <ChevronDown size={13} className="shrink-0" />
      </Button>
    </Popover.Trigger>
    <Popover.Portal>
      <Popover.Content align="start" sideOffset={7} className="work-project-popover" aria-label="Registered checkouts">
        <h2 className="text-[13px] font-semibold">Repositories and worktrees</h2>
        <label className="work-search"><Search size={16} aria-hidden="true" />
          <input type="search" aria-label="Search repositories and worktrees" placeholder="Search name or path…"
            value={query} onChange={(event) => setQuery(event.currentTarget.value)} />
        </label>
        {registered.isPending && <p role="status">Loading checkouts…</p>}
        {(error || registered.error) && <p role="alert">{error || String(registered.error)}</p>}
        {status && <p role="status">{status}</p>}
        {registered.data?.length === 0 && <p>No registered repositories yet.</p>}
        {Boolean(registered.data?.length) && !results.length && <p role="status">No matching checkouts.</p>}
        {pending && <p role="status">Opening checkout…</p>}
        <ul>
          {results.map((item) => {
            const selected = item.checkoutId === checkout?.checkoutId;
            const favorite = favorites.has(item.projectId);
            const name = basename(item.repositoryRoot) + (item.checkoutRoot === item.repositoryRoot ? "" : " / " + basename(item.checkoutRoot));
            const actions = fileContextActions({ checkout: item, status: setStatus,
              report: (message) => { setStatus(""); setError(message); } });
            return <li key={item.checkoutId}>
              <ContextActions label={item.checkoutRoot} groups={[[
                { label: "Use this worktree", disabled: selected || pending, run: () => void select(item) },
                ...checkoutAreas.map(([workspace, title]) => ({
                  label: `Open in ${title}`, disabled: pending,
                  run: () => void select(item, workspace === "git" ? { workspace, target: gitCheckoutTarget() } : { workspace }),
                })),
                { label: "Open terminal here", disabled: pending, run: () => void select(item, { workspace: "ide", target: terminalFolderTarget("") }) },
                { label: "Create worktree…", disabled: pending, run: () => void select(item, { workspace: "git", target: newWorktreeTarget() }) },
              ], [
                { label: favorite ? "Remove project from favorites" : "Favorite project", disabled: pending, run: () => toggleFavorite(item) },
                { label: "Copy repository path", run: () => actions.copy(item.repositoryRoot, "Copied repository path") },
                { label: "Copy checkout ID", run: () => actions.copy(item.checkoutId, "Copied checkout ID") },
              ], ...actions.location({ kind: "root", path: "" })]}>
                <button className="work-project-option" type="button" aria-current={selected ? "true" : undefined}
                  aria-label={`${name} · ${item.checkoutRoot}`} aria-description={favorite ? "Favorite project" : undefined}
                  disabled={pending} onClick={() => void select(item)} title={item.checkoutRoot}>
                  {item.checkoutRoot === item.repositoryRoot ? <FolderGit2 size={16} /> : <GitFork size={16} />}
                  <span className="grow"><strong>{name}</strong><small>{item.checkoutRoot}</small></span>
                  {favorite && <Star aria-hidden size={14} className="shrink-0 fill-current text-primary" />}
                  {selected && <Check size={16} className="shrink-0" aria-hidden />}
                </button>
              </ContextActions>
            </li>;
          })}
        </ul>
      </Popover.Content>
    </Popover.Portal>
  </Popover.Root>;
}
