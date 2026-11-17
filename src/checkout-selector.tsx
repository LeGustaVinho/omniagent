import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { ChevronDown, FolderGit2, GitFork } from "lucide-react";
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

export function CheckoutSelector({ checkout, onSelect, disabled }: {
  checkout: ProjectCheckout | null;
  disabled?: boolean;
  onSelect: (checkout: ProjectCheckout, destination?: WorkspaceDestination) => Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const [status, setStatus] = useState("");
  const registered = useQuery({
    queryKey: ["registered-checkouts"],
    queryFn: () => invoke<ProjectCheckout[]>("registered_checkouts"),
    enabled: open,
  });

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

  return <Popover.Root open={open} onOpenChange={setOpen}>
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
        {registered.isPending && <p role="status">Loading checkouts…</p>}
        {(error || registered.error) && <p role="alert">{error || String(registered.error)}</p>}
        {status && <p role="status">{status}</p>}
        {registered.data?.length === 0 && <p>No registered repositories yet.</p>}
        <ul>
          {registered.data?.map((item) => {
            const selected = item.checkoutId === checkout?.checkoutId;
            const actions = fileContextActions({ checkout: item, simulated: false, status: setStatus,
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
                { label: "Copy repository path", run: () => actions.copy(item.repositoryRoot, "Copied repository path") },
                { label: "Copy checkout ID", run: () => actions.copy(item.checkoutId, "Copied checkout ID") },
              ], ...actions.location({ kind: "root", path: "" })]}>
                <button className="work-project-option" type="button" aria-current={selected ? "true" : undefined}
                  disabled={pending} onClick={() => void select(item)} title={item.checkoutRoot}>
                  {item.checkoutRoot === item.repositoryRoot ? <FolderGit2 size={16} /> : <GitFork size={16} />}
                  <span><strong>{basename(item.repositoryRoot)}</strong><small>{item.checkoutRoot}</small></span>
                </button>
              </ContextActions>
            </li>;
          })}
        </ul>
      </Popover.Content>
    </Popover.Portal>
  </Popover.Root>;
}
