import { invoke } from "@tauri-apps/api/core";
import { useQuery } from "@tanstack/react-query";
import { ChevronDown, FolderGit2, GitFork } from "lucide-react";
import { Popover } from "radix-ui";
import { useState } from "react";
import type { ProjectCheckout } from "@/bindings/ProjectCheckout";
import { Button } from "@/components/ui/button";

function basename(path: string) {
  return path.split(/[\\/]/).filter(Boolean).at(-1) ?? path;
}

export function CheckoutSelector({ checkout, onSelect, disabled }: {
  checkout: ProjectCheckout | null;
  disabled?: boolean;
  onSelect: (checkout: ProjectCheckout) => Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState(false);
  const [error, setError] = useState("");
  const registered = useQuery({
    queryKey: ["registered-checkouts"],
    queryFn: () => invoke<ProjectCheckout[]>("registered_checkouts"),
    enabled: open,
  });

  async function select(checked: ProjectCheckout) {
    setPending(true);
    setError("");
    try {
      await onSelect(checked);
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
        {registered.data?.length === 0 && <p>No registered repositories yet.</p>}
        <ul>
          {registered.data?.map((item) => <li key={item.checkoutId}>
            <button type="button" aria-current={item.checkoutId === checkout?.checkoutId ? "true" : undefined}
              disabled={pending} onClick={() => void select(item)} title={item.checkoutRoot}>
              {item.checkoutRoot === item.repositoryRoot ? <FolderGit2 size={16} /> : <GitFork size={16} />}
              <span><strong>{basename(item.repositoryRoot)}</strong><small>{item.checkoutRoot}</small></span>
            </button>
          </li>)}
        </ul>
      </Popover.Content>
    </Popover.Portal>
  </Popover.Root>;
}
