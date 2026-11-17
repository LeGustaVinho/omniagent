import { X } from "lucide-react";
import { Dialog } from "radix-ui";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { InlineIssue } from "@/tasks-git-ui";

type Option = { value: string; label: string; disabled?: boolean };
export type WorktreeDraft = { branch: string; existing: boolean; base: string; path: string };

export function GitWorktreeDialog({ revisions, defaultBase, existingBranches, pathHint, defaultPath, busy, error,
  validate, create, onClose }: {
  revisions: readonly Option[];
  defaultBase: string;
  existingBranches?: readonly Option[];
  pathHint: string;
  defaultPath?: (branch: string) => string;
  busy?: boolean;
  error?: string;
  validate: (draft: WorktreeDraft) => string | undefined;
  create: (draft: WorktreeDraft) => void;
  onClose: () => void;
}) {
  const [branchName, setBranchName] = useState("");
  const [branchSelect, setBranchSelect] = useState("");
  const [existing, setExisting] = useState(false);
  const [base, setBase] = useState(defaultBase);
  const [path, setPath] = useState("");
  const branch = existing ? branchSelect : branchName.trim();
  const destination = path.trim() || defaultPath?.(branch) || "";
  const draft = { branch, existing, base, path: destination };
  const issue = branch ? validate(draft) : undefined;
  const canCreate = Boolean(branch && !issue && !busy);
  return <Dialog.Root open onOpenChange={(open) => { if (!open && !busy) onClose(); }}>
    <Dialog.Portal>
      <Dialog.Overlay className="fixed inset-0 z-50 bg-black/60" />
      <Dialog.Content className="fixed left-1/2 top-1/2 z-50 max-h-[calc(100dvh-2rem)] w-[min(31rem,calc(100vw-2rem))] -translate-x-1/2 -translate-y-1/2 overflow-auto rounded-xl border border-border bg-card p-5 shadow-xl">
        <header className="mb-3 flex items-center justify-between gap-2">
          <Dialog.Title className="font-semibold">New worktree</Dialog.Title>
          <Dialog.Close aria-label="Close worktree setup" disabled={busy}><X size={16} /></Dialog.Close>
        </header>
        <Dialog.Description className="mb-4 text-sm text-muted-foreground">Choose the branch, starting revision and path for the new checkout.</Dialog.Description>
        <form className="grid gap-3" onSubmit={(event) => { event.preventDefault(); if (canCreate) create(draft); }}>
          {existingBranches && <label className="delivery-field">Branch mode
            <select aria-label="Branch mode" className="delivery-filter" value={existing ? "existing" : "new"} disabled={busy}
              onChange={(event) => setExisting(event.currentTarget.value === "existing")}>
              <option value="new">Create a branch</option>
              <option value="existing" disabled={!existingBranches.some((option) => !option.disabled)}>Use existing branch</option>
            </select>
          </label>}
          <label className="delivery-field">Branch{existing
            ? <select aria-label="Branch" className="delivery-filter" required value={branchSelect} disabled={busy}
              onChange={(event) => setBranchSelect(event.currentTarget.value)}>
              <option value="">Select a local branch</option>
              {existingBranches?.map((option) => <option key={option.value} value={option.value} disabled={option.disabled}>{option.label}</option>)}
            </select>
            : <input aria-label="Branch" className="delivery-filter" required placeholder="agent/my-task" value={branchName} disabled={busy}
              onChange={(event) => setBranchName(event.currentTarget.value)} />}
          </label>
          {!existing && <label className="delivery-field">Base revision
            <select aria-label="Base revision" className="delivery-filter" value={base} disabled={busy} onChange={(event) => setBase(event.currentTarget.value)}>
              {revisions.map((entry) => <option key={entry.value} value={entry.value} disabled={entry.disabled}>{entry.label}</option>)}
            </select>
          </label>}
          <label className="delivery-field">Worktree path
            <input aria-label="Worktree path" className="delivery-filter" placeholder={defaultPath?.(branch) || pathHint} value={path} disabled={busy}
              onChange={(event) => setPath(event.currentTarget.value)} />
          </label>
          {branch && <p className="break-all text-xs text-muted-foreground">Preview <code>
            {!existing && (revisions.find(({ value }) => value === base)?.label ?? base) + " → "}
            {branch} → {destination || pathHint}
          </code></p>}
          {(issue || error) && <InlineIssue>{issue || error}</InlineIssue>}
          <Button type="submit" className="justify-self-start px-2.5 py-1.5 text-xs" disabled={!canCreate}>{busy ? "Creating worktree…" : "Create worktree"}</Button>
        </form>
      </Dialog.Content>
    </Dialog.Portal>
  </Dialog.Root>;
}
